# tgvault Telegram setup wizard. Writes .env next to this repo and verifies it.
# Secrets are read with Read-Host -AsSecureString (not echoed) and only written to .env.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$bin = Join-Path $root "target\release\tgvault.exe"
if (-not (Test-Path $bin)) {
    Write-Host "Building tgvault (first run only)..."
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

function Read-Secret([string]$prompt) {
    $s = Read-Host -Prompt $prompt -AsSecureString
    [Runtime.InteropServices.Marshal]::PtrToStringBSTR([Runtime.InteropServices.Marshal]::SecureStringToBSTR($s))
}

if (Test-Path ".env") {
    $ans = Read-Host ".env already exists. Overwrite it? Existing master keys will be lost (y/N)"
    if ($ans -ne "y") { Write-Host "Kept the existing .env. Run: $bin telegram-check"; exit 0 }
}

Write-Host "`nStep 1. Paste your bot token(s) from @BotFather, comma separated."
$tokens = (Read-Secret "Bot token(s)").Trim()
if (-not $tokens) { throw "No token given." }
$env:TGV_BOT_TOKENS = $tokens

Write-Host "`nStep 2. Looking for channels your bots have seen..."
Write-Host "(The bot must be an admin of your private channel, and you must have posted one message there.)"
& $bin telegram-chats
$chat = Read-Host "`nType the channel id from the list above (starts with -100)"
if ($chat -notmatch '^-100\d+$') { Write-Warning "That does not look like a channel id; continuing anyway." }

$secrets = & $bin gen-secrets
$master = ($secrets | Select-String '^TGV_MASTER_KEYS=').Line
$server = ($secrets | Select-String '^TGV_SERVER_SECRET=').Line

$admin = Read-Host "`nStep 3. Choose a panel password (12+ characters), or press Enter to generate one"
if (-not $admin) {
    $bytes = New-Object byte[] 18; [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    $admin = [Convert]::ToBase64String($bytes) -replace '[+/=]', 'x'
    Write-Host "Generated panel password: $admin   (write it down)"
}
if ($admin.Length -lt 12) { throw "The panel password must be at least 12 characters." }

$lines = @(
    "TGV_BACKEND=telegram",
    "TGV_BOT_TOKENS=$tokens",
    "TGV_CHAT_ID=$chat",
    $master,
    $server,
    "TGV_ADMIN_PASSWORD=$admin",
    "TGV_BIND=127.0.0.1:8787",
    "TGV_PUBLIC_URL=http://127.0.0.1:8787",
    "TGV_DATA_DIR=./data",
    "TGV_CHUNK_MIB=16"
)
$utf8 = New-Object System.Text.UTF8Encoding $false
[IO.File]::WriteAllLines((Join-Path $root ".env"), $lines, $utf8)
# Keep .env readable by the current user only.
icacls ".env" /inheritance:r /grant:r "$($env:USERNAME):(R,W)" | Out-Null
Write-Host "`nWrote .env (only your Windows user can read it). Back up TGV_MASTER_KEYS somewhere safe: without it the data cannot be decrypted."

Write-Host "`nStep 4. Checking the setup end to end..."
& (Join-Path $PSScriptRoot "run.ps1") -Command telegram-check
