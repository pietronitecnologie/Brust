// Example local software built with BrustClient.cs: a test bench (.NET 6+).
// NOT compiled in the environment it was written in – build and test before use.
//
//   dotnet new console -n Tester && copy BrustClient.cs + ExampleTester.cs into it
//   (delete the generated Program.cs), then:
//   set BRUST_TOKEN=<token from Brust Settings>
//   dotnet run -- tester
//
// A web app starts a test with
//   brust.call("tester", "startTest", { v: 1, code: "PZ-001", repetitions: 3 }, { timeout_ms: 60000 })
// receives "test.progress" events and gets back { code, outcome: "OK", measurements: [...] }.

#nullable enable

using System;
using System.Collections.Generic;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;
using Brust.Client;

public static class ExampleTester
{
    private static int _busy; // the instrument runs one test at a time

    public static async Task Main(string[] args)
    {
        string name = args.Length > 0 ? args[0] : "tester";
        var client = new BrustClient(token: null, name: name) { Log = Console.WriteLine };

        client.RegisterMethod("startTest", async (p, ctx) =>
        {
            int version = (int?)p?["v"] ?? 1;
            if (version != 1) throw new Exception($"unsupported version: {version}");
            string code = (string?)p?["code"] ?? throw new Exception("missing 'code'");
            int repetitions = (int?)p?["repetitions"] ?? 1;
            if (repetitions < 1 || repetitions > 100) throw new Exception("'repetitions' must be between 1 and 100");
            if (Interlocked.CompareExchange(ref _busy, 1, 0) != 0) throw new Exception("BUSY: test already running");
            try
            {
                Console.WriteLine($"test {code} x{repetitions} requested by {ctx.Caller}");
                var measurements = new List<double>();
                for (int i = 0; i < repetitions; i++)
                {
                    await Task.Delay(1000, ctx.CancellationToken); // the real measurement goes here
                    measurements.Add(Math.Round(12.0 + i * 0.1, 3));
                    await ctx.EmitToCallerAsync("test.progress", new
                    {
                        code,
                        step = i + 1,
                        percent = (int)Math.Round((i + 1) * 100.0 / repetitions),
                    });
                }
                return new { code, outcome = "OK", measurements };
            }
            finally
            {
                Interlocked.Exchange(ref _busy, 0);
            }
        });

        client.RegisterMethod("status", (p, ctx) => Task.FromResult<object?>(new { busy = _busy != 0 }));

        client.Connected += welcome => Console.WriteLine($"connected to Brust {welcome["version"]}");
        client.On("*", (data, source) => Console.WriteLine($"event from {source}: {data?.ToJsonString()}"));

        using var cts = new CancellationTokenSource();
        Console.CancelKeyPress += (_, e) => { e.Cancel = true; cts.Cancel(); };
        await client.RunAsync(cts.Token);
    }
}
