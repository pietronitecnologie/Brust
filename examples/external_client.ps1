# Example: local software that talks to the apps open in Brust.
# Usage: powershell -File external_client.ps1 -Token <token from Settings>
param(
    [Parameter(Mandatory = $true)][string]$Token,
    [int]$Port = 47800
)

$ws = [System.Net.WebSockets.ClientWebSocket]::new()
$uri = [Uri]"ws://127.0.0.1:$Port/?token=$Token&name=script-powershell"
$ws.ConnectAsync($uri, [Threading.CancellationToken]::None).Wait()

function Send($obj) {
    $json = $obj | ConvertTo-Json -Compress -Depth 10
    $bytes = [Text.Encoding]::UTF8.GetBytes($json)
    $seg = [ArraySegment[byte]]::new($bytes)
    $ws.SendAsync($seg, 'Text', $true, [Threading.CancellationToken]::None).Wait()
}

function Receive() {
    $buf = New-Object byte[] 65536
    $seg = [ArraySegment[byte]]::new($buf)
    $r = $ws.ReceiveAsync($seg, [Threading.CancellationToken]::None).Result
    [Text.Encoding]::UTF8.GetString($buf, 0, $r.Count) | ConvertFrom-Json
}

Receive | Out-Host                                  # welcome message

# 1) Event to all apps (brust.on("order.created", ...))
Send @{ type = "emit"; target = "*"; event = "order.created"; data = @{ number = 123 } }
Receive | Out-Host

# 2) Call to a method registered by Ordo with brust.handle("sum", ...)
Send @{ type = "call"; id = 1; target = "ordo"; method = "sum"; params = @{ a = 20; b = 22 } }
do { $m = Receive } while ($m.type -ne "result" -or $m.id -ne 1)
"Result: $($m.result)"

$ws.CloseAsync('NormalClosure', "done", [Threading.CancellationToken]::None).Wait()
