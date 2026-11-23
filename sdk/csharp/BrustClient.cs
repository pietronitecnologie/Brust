// Brust bridge client for local software – single file, .NET 6+ (C# 10).
// Dependencies: none beyond the BCL (System.Net.WebSockets, System.Text.Json).
//
// NOTE: this file has not been compiled in the environment it was written in
// (no .NET SDK available). Please build and test it before relying on it.
//
//   var client = new BrustClient(token: null, name: "tester");   // token from BRUST_TOKEN
//   client.RegisterMethod("startTest", async (p, ctx) => {
//       await ctx.EmitToCallerAsync("test.progress", new { percent = 50 });
//       return new { outcome = "OK" };
//   });
//   client.On("order.saved", (data, source) => Console.WriteLine($"{source}: {data}"));
//   await client.RunAsync(cancellationToken);
//
// Protocol: see README.md / GUIDE.md of Brust ("Bridge for local software").

#nullable enable

using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.IO;
using System.Net.WebSockets;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;

namespace Brust.Client
{
    /// <summary>An error returned by Brust or by the remote side of a call.</summary>
    public sealed class BrustException : Exception
    {
        public BrustException(string message) : base(message) { }
    }

    /// <summary>Context passed to a method handler.</summary>
    public sealed class CallContext
    {
        private readonly BrustClient _client;

        internal CallContext(BrustClient client, long id, string method, string caller, CancellationToken cancellationToken)
        {
            _client = client;
            Id = id;
            Method = method;
            Caller = caller;
            CancellationToken = cancellationToken;
        }

        /// <summary>Id of the call (assigned by Brust).</summary>
        public long Id { get; }

        public string Method { get; }

        /// <summary>The "from" field of the call: an app id ("ordo") or "external:&lt;name&gt;".</summary>
        public string Caller { get; }

        /// <summary>Cancelled when the client is stopped.</summary>
        public CancellationToken CancellationToken { get; }

        /// <summary>Sends an event to the app (or program) that made the call.</summary>
        public Task EmitToCallerAsync(string eventName, object? data = null)
            => _client.EmitAsync(eventName, data, string.IsNullOrEmpty(Caller) ? "*" : Caller);
    }

    /// <summary>Client for the Brust local WebSocket bridge.</summary>
    public sealed class BrustClient : IAsyncDisposable
    {
        private readonly ConcurrentDictionary<string, Func<JsonNode?, CallContext, Task<object?>>> _methods = new();
        private readonly Dictionary<string, List<Action<JsonNode?, string>>> _listeners = new();
        private readonly object _listenersLock = new();
        private readonly ConcurrentDictionary<long, TaskCompletionSource<JsonNode?>> _pending = new();
        private readonly SemaphoreSlim _sendLock = new(1, 1);
        private readonly object _stateLock = new();
        private TaskCompletionSource<bool> _connectedSignal = NewSignal();
        private volatile ClientWebSocket? _ws;
        private long _nextId;
        private CancellationToken _runToken;

        /// <param name="token">Bridge token (Brust → Settings). If null, read from the BRUST_TOKEN environment variable.</param>
        /// <param name="name">Client name: apps call this program with brust.call(name, ...).</param>
        /// <param name="port">Bridge port (default 47800).</param>
        /// <param name="host">Bridge host (Brust only listens on 127.0.0.1).</param>
        public BrustClient(string? token, string name, int port = 47800, string host = "127.0.0.1")
        {
            token ??= Environment.GetEnvironmentVariable("BRUST_TOKEN");
            if (string.IsNullOrEmpty(token))
                throw new ArgumentException("Brust token missing: pass it or set BRUST_TOKEN", nameof(token));
            Token = token;
            Name = name;
            Port = port;
            Host = host;
        }

        public string Token { get; }
        public string Name { get; }
        public int Port { get; }
        public string Host { get; }

        public TimeSpan MinBackoff { get; set; } = TimeSpan.FromSeconds(0.5);
        public TimeSpan MaxBackoff { get; set; } = TimeSpan.FromSeconds(15);
        public TimeSpan DefaultTimeout { get; set; } = TimeSpan.FromSeconds(30);

        /// <summary>The last "welcome" message received.</summary>
        public JsonObject? Welcome { get; private set; }

        /// <summary>Raised after every (re)connection, with the welcome message.</summary>
        public event Action<JsonObject>? Connected;

        /// <summary>Raised when the connection drops (the exception, if any, that caused it).</summary>
        public event Action<Exception?>? Disconnected;

        /// <summary>Optional log sink.</summary>
        public Action<string>? Log { get; set; }

        public bool IsConnected => _ws?.State == WebSocketState.Open;

        public Uri Url => new Uri(
            $"ws://{Host}:{Port}/?token={Uri.EscapeDataString(Token)}&name={Uri.EscapeDataString(Name)}");

        /* ---------------------------------------------------- registration */

        /// <summary>Registers a method callable by web apps with brust.call(Name, method, params).</summary>
        public void RegisterMethod(string name, Func<JsonNode?, CallContext, Task<object?>> handler)
        {
            _methods[name] = handler ?? throw new ArgumentNullException(nameof(handler));
        }

        /// <summary>Listens for an event ("*" = all). The handler receives (data, source). Dispose the result to unsubscribe.</summary>
        public IDisposable On(string eventName, Action<JsonNode?, string> handler)
        {
            lock (_listenersLock)
            {
                if (!_listeners.TryGetValue(eventName, out var list))
                {
                    list = new List<Action<JsonNode?, string>>();
                    _listeners[eventName] = list;
                }
                list.Add(handler);
            }
            return new Subscription(() =>
            {
                lock (_listenersLock)
                {
                    if (_listeners.TryGetValue(eventName, out var l)) l.Remove(handler);
                }
            });
        }

        /* -------------------------------------------------------- requests */

        /// <summary>Sends an event to an app (target = app id) or to all ("*"). Does not wait for an acknowledgement.</summary>
        public Task EmitAsync(string eventName, object? data = null, string target = "*", CancellationToken cancellationToken = default)
        {
            var msg = new JsonObject
            {
                ["type"] = "emit",
                ["id"] = Interlocked.Increment(ref _nextId), // the acknowledgement is ignored
                ["target"] = target,
                ["event"] = eventName,
                ["data"] = ToNode(data),
            };
            return SendAsync(msg, cancellationToken);
        }

        /// <summary>Calls a method registered with brust.handle by the app <paramref name="target"/>.</summary>
        /// <exception cref="BrustException">The call failed on the other side.</exception>
        /// <exception cref="TimeoutException">No reply within the timeout.</exception>
        public Task<JsonNode?> CallAsync(string target, string method, object? parameters = null,
            TimeSpan? timeout = null, CancellationToken cancellationToken = default)
        {
            var msg = new JsonObject
            {
                ["type"] = "call",
                ["target"] = target,
                ["method"] = method,
                ["params"] = ToNode(parameters),
            };
            return RequestAsync(msg, timeout, cancellationToken);
        }

        /// <summary>Tabs, panes and configured apps.</summary>
        public Task<JsonNode?> ListAsync(TimeSpan? timeout = null, CancellationToken cancellationToken = default)
            => RequestAsync(new JsonObject { ["type"] = "list" }, timeout, cancellationToken);

        /// <summary>Opens an app (optionally at a url) or a web address in a new tab.</summary>
        public Task<JsonNode?> OpenAsync(string? app = null, string? url = null,
            TimeSpan? timeout = null, CancellationToken cancellationToken = default)
        {
            if (string.IsNullOrEmpty(app) && string.IsNullOrEmpty(url))
                throw new ArgumentException("specify app or url");
            var msg = new JsonObject { ["type"] = "open" };
            if (!string.IsNullOrEmpty(app)) msg["app"] = app;
            if (!string.IsNullOrEmpty(url)) msg["url"] = url;
            return RequestAsync(msg, timeout, cancellationToken);
        }

        /// <summary>Brings an open app to the front.</summary>
        public Task<JsonNode?> FocusAsync(string app, TimeSpan? timeout = null, CancellationToken cancellationToken = default)
            => RequestAsync(new JsonObject { ["type"] = "focus", ["app"] = app }, timeout, cancellationToken);

        /// <summary>Waits until the client is connected.</summary>
        public async Task WaitConnectedAsync(TimeSpan timeout, CancellationToken cancellationToken = default)
        {
            Task signal;
            lock (_stateLock) signal = _connectedSignal.Task;
            if (IsConnected) return;
            var done = await Task.WhenAny(signal, Task.Delay(timeout, cancellationToken)).ConfigureAwait(false);
            cancellationToken.ThrowIfCancellationRequested();
            if (done != signal) throw new TimeoutException("not connected to Brust");
        }

        private async Task<JsonNode?> RequestAsync(JsonObject msg, TimeSpan? timeout, CancellationToken cancellationToken)
        {
            var limit = timeout ?? DefaultTimeout;
            using var cts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            cts.CancelAfter(limit);

            await WaitConnectedAsync(limit, cancellationToken).ConfigureAwait(false);

            long id = Interlocked.Increment(ref _nextId);
            msg["id"] = id;
            var tcs = new TaskCompletionSource<JsonNode?>(TaskCreationOptions.RunContinuationsAsynchronously);
            _pending[id] = tcs;
            string what = Str(msg["method"]) ?? Str(msg["type"]) ?? "request";
            using var registration = cts.Token.Register(() =>
            {
                if (cancellationToken.IsCancellationRequested) tcs.TrySetCanceled(cancellationToken);
                else tcs.TrySetException(new TimeoutException($"timeout: no reply to '{what}' in {limit.TotalSeconds:0.###} s"));
            });
            try
            {
                await SendAsync(msg, cts.Token).ConfigureAwait(false);
                return await tcs.Task.ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested && cts.IsCancellationRequested)
            {
                // Cancelled while sending because the timeout elapsed.
                throw new TimeoutException($"timeout: no reply to '{what}' in {limit.TotalSeconds:0.###} s");
            }
            finally
            {
                _pending.TryRemove(id, out _);
            }
        }

        private async Task SendAsync(JsonObject msg, CancellationToken cancellationToken)
        {
            var bytes = Encoding.UTF8.GetBytes(msg.ToJsonString());
            await _sendLock.WaitAsync(cancellationToken).ConfigureAwait(false);
            try
            {
                var ws = _ws;
                if (ws == null || ws.State != WebSocketState.Open)
                    throw new InvalidOperationException("not connected to Brust");
                await ws.SendAsync(new ArraySegment<byte>(bytes), WebSocketMessageType.Text, true, cancellationToken)
                    .ConfigureAwait(false);
            }
            finally
            {
                _sendLock.Release();
            }
        }

        /* ------------------------------------------------------- main loop */

        /// <summary>Connects and serves until cancelled, reconnecting with exponential backoff.</summary>
        public async Task RunAsync(CancellationToken cancellationToken = default)
        {
            _runToken = cancellationToken;
            var backoff = MinBackoff;
            var random = new Random();
            while (!cancellationToken.IsCancellationRequested)
            {
                Exception? error = null;
                using (var ws = new ClientWebSocket())
                {
                    ws.Options.KeepAliveInterval = TimeSpan.FromSeconds(20);
                    try
                    {
                        using (var connectCts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken))
                        {
                            connectCts.CancelAfter(TimeSpan.FromSeconds(10));
                            await ws.ConnectAsync(Url, connectCts.Token).ConfigureAwait(false);
                        }
                        _ws = ws;
                        backoff = MinBackoff;
                        Log?.Invoke($"connected to Brust as '{Name}'");
                        lock (_stateLock) _connectedSignal.TrySetResult(true);
                        await ReceiveLoopAsync(ws, cancellationToken).ConfigureAwait(false);
                    }
                    catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
                    {
                        // stopping
                    }
                    catch (Exception e)
                    {
                        error = e;
                        Log?.Invoke($"Brust unreachable: {e.Message}");
                    }
                    finally
                    {
                        bool wasConnected = _ws == ws;
                        OnConnectionLost();
                        if (ws.State == WebSocketState.Open || ws.State == WebSocketState.CloseReceived)
                        {
                            try
                            {
                                using var closeCts = new CancellationTokenSource(TimeSpan.FromSeconds(2));
                                await ws.CloseAsync(WebSocketCloseStatus.NormalClosure, "bye", closeCts.Token).ConfigureAwait(false);
                            }
                            catch
                            {
                                // ignore
                            }
                        }
                        if (wasConnected)
                        {
                            try { Disconnected?.Invoke(error); } catch (Exception e) { Log?.Invoke($"Disconnected handler failed: {e.Message}"); }
                        }
                    }
                }

                if (cancellationToken.IsCancellationRequested) break;
                var jitter = 0.8 + 0.4 * random.NextDouble();
                var delay = TimeSpan.FromMilliseconds(backoff.TotalMilliseconds * jitter);
                try
                {
                    await Task.Delay(delay, cancellationToken).ConfigureAwait(false);
                }
                catch (OperationCanceledException)
                {
                    break;
                }
                backoff = TimeSpan.FromMilliseconds(Math.Min(backoff.TotalMilliseconds * 2, MaxBackoff.TotalMilliseconds));
            }
        }

        public ValueTask DisposeAsync()
        {
            var ws = _ws;
            ws?.Abort();
            _sendLock.Dispose();
            return ValueTask.CompletedTask;
        }

        private async Task ReceiveLoopAsync(ClientWebSocket ws, CancellationToken cancellationToken)
        {
            var buffer = new byte[16 * 1024];
            using var message = new MemoryStream();
            while (ws.State == WebSocketState.Open)
            {
                message.SetLength(0);
                WebSocketReceiveResult result;
                do
                {
                    result = await ws.ReceiveAsync(new ArraySegment<byte>(buffer), cancellationToken).ConfigureAwait(false);
                    if (result.MessageType == WebSocketMessageType.Close)
                    {
                        Log?.Invoke($"Brust closed the connection: {result.CloseStatus} {result.CloseStatusDescription}");
                        return;
                    }
                    message.Write(buffer, 0, result.Count);
                } while (!result.EndOfMessage);

                if (result.MessageType != WebSocketMessageType.Text) continue;
                var text = Encoding.UTF8.GetString(message.GetBuffer(), 0, (int)message.Length);
                Dispatch(text);
            }
        }

        private void OnConnectionLost()
        {
            _ws = null;
            lock (_stateLock)
            {
                if (_connectedSignal.Task.IsCompleted) _connectedSignal = NewSignal();
            }
            foreach (var entry in _pending)
            {
                entry.Value.TrySetException(new IOException("connection to Brust lost"));
            }
            _pending.Clear();
        }

        /* -------------------------------------------------------- dispatch */

        private void Dispatch(string text)
        {
            JsonObject? msg;
            try
            {
                msg = JsonNode.Parse(text) as JsonObject;
            }
            catch (JsonException)
            {
                Log?.Invoke("invalid JSON received from Brust");
                return;
            }
            if (msg == null) return;

            switch (Str(msg["type"]))
            {
                case "call":
                {
                    long id = Long(msg["id"]);
                    string method = Str(msg["method"]) ?? "";
                    string caller = Str(msg["from"]) ?? "*";
                    JsonNode? parameters = Detach(msg, "params");
                    _ = Task.Run(() => HandleCallAsync(id, method, caller, parameters));
                    break;
                }
                case "result":
                case "pong":
                {
                    long id = Long(msg["id"]);
                    if (!_pending.TryRemove(id, out var tcs)) return;
                    bool ok = !(msg["ok"] is JsonValue okValue && okValue.TryGetValue<bool>(out var b) && !b);
                    if (ok) tcs.TrySetResult(Detach(msg, "result"));
                    else tcs.TrySetException(new BrustException(Str(msg["error"]) ?? "error"));
                    break;
                }
                case "event":
                {
                    string eventName = Str(msg["event"]) ?? "";
                    string source = Str(msg["source"]) ?? "";
                    JsonNode? data = Detach(msg, "data");
                    var handlers = new List<Action<JsonNode?, string>>();
                    lock (_listenersLock)
                    {
                        if (_listeners.TryGetValue(eventName, out var l1)) handlers.AddRange(l1);
                        if (eventName != "*" && _listeners.TryGetValue("*", out var l2)) handlers.AddRange(l2);
                    }
                    foreach (var h in handlers)
                    {
                        try { h(data, source); }
                        catch (Exception e) { Log?.Invoke($"event handler for '{eventName}' failed: {e.Message}"); }
                    }
                    break;
                }
                case "welcome":
                    Welcome = msg;
                    try { Connected?.Invoke(msg); }
                    catch (Exception e) { Log?.Invoke($"Connected handler failed: {e.Message}"); }
                    break;
                case "error":
                    Log?.Invoke($"Brust error: {Str(msg["error"])}");
                    break;
            }
        }

        private async Task HandleCallAsync(long id, string method, string caller, JsonNode? parameters)
        {
            JsonObject reply;
            try
            {
                if (!_methods.TryGetValue(method, out var handler))
                    throw new BrustException($"unknown method: {method}");
                var ctx = new CallContext(this, id, method, caller, _runToken);
                object? result = await handler(parameters, ctx).ConfigureAwait(false);
                reply = new JsonObject { ["type"] = "reply", ["id"] = id, ["ok"] = true, ["result"] = ToNode(result) };
            }
            catch (Exception e)
            {
                // The error reaches the app as a rejected Promise.
                reply = new JsonObject { ["type"] = "reply", ["id"] = id, ["ok"] = false, ["error"] = e.Message };
            }
            try
            {
                await SendAsync(reply, CancellationToken.None).ConfigureAwait(false);
            }
            catch (Exception e)
            {
                Log?.Invoke($"could not reply to call {id}: {e.Message}");
            }
        }

        /* --------------------------------------------------------- helpers */

        private static TaskCompletionSource<bool> NewSignal()
            => new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);

        /// <summary>Converts any value to a parent-less JsonNode (JsonNode values are copied).</summary>
        private static JsonNode? ToNode(object? value)
        {
            if (value == null) return null;
            if (value is JsonNode node)
                return node.Parent == null ? node : JsonNode.Parse(node.ToJsonString());
            return JsonSerializer.SerializeToNode(value, value.GetType());
        }

        private static JsonNode? Detach(JsonObject obj, string key)
        {
            if (!obj.TryGetPropertyValue(key, out var node)) return null;
            obj.Remove(key);
            return node;
        }

        private static string? Str(JsonNode? node)
            => node is JsonValue v && v.TryGetValue<string>(out var s) ? s : null;

        private static long Long(JsonNode? node)
        {
            if (node is JsonValue v)
            {
                if (v.TryGetValue<long>(out var l)) return l;
                if (v.TryGetValue<double>(out var d)) return (long)d;
            }
            return 0;
        }

        private sealed class Subscription : IDisposable
        {
            private Action? _dispose;
            public Subscription(Action dispose) { _dispose = dispose; }
            public void Dispose() { Interlocked.Exchange(ref _dispose, null)?.Invoke(); }
        }
    }
}
