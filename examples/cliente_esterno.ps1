# Esempio: software locale che dialoga con gli applicativi aperti in Brust.
# Uso: powershell -File cliente_esterno.ps1 -Token <token da Impostazioni>
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

Receive | Out-Host                                  # messaggio di benvenuto

# 1) Evento verso tutte le app (brust.on("ordine.nuovo", ...))
Send @{ type = "emit"; target = "*"; event = "ordine.nuovo"; data = @{ numero = 123 } }
Receive | Out-Host

# 2) Chiamata a un metodo registrato da Ordo con brust.handle("somma", ...)
Send @{ type = "call"; id = 1; target = "ordo"; method = "somma"; params = @{ a = 20; b = 22 } }
do { $m = Receive } while ($m.type -ne "result" -or $m.id -ne 1)
"Risultato: $($m.result)"

$ws.CloseAsync('NormalClosure', "fine", [Threading.CancellationToken]::None).Wait()
