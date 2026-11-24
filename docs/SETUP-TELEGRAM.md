# Connecting tgvault to Telegram

About five minutes. You need a Telegram account and the Telegram app.

## 1. Create 1 to 4 bots

1. Open a chat with [@BotFather](https://t.me/BotFather), send `/newbot`, and pick a name and a username that ends in `bot`.
2. BotFather replies with a **token** that looks like `123456789:AAH…`. It is the bot's password. Put it only in your local `.env`. Never paste it into an issue, a chat or a commit.
3. Optional: repeat for 2 to 4 bots. More bots upload in parallel and are less likely to hit Telegram's flood limits (HTTP 429).

## 2. Create the private channel that holds your data

1. Telegram → **New Channel** → name it (for example `my-vault`) → choose **Private**.
2. Open the channel → **Administrators** → **Add Admin** → pick each bot → enable **Post messages**, **Delete messages** and **Pin messages** (or *Edit messages*) → Save.
3. Post any message in the channel (for example `hello`). This lets the bot see the channel so tgvault can read its id.

## 3. Configure and verify

### Guided (Windows PowerShell)

```powershell
.\scripts\setup.ps1
```

It asks for the token(s) without echoing them, finds the channel id, generates the encryption keys and an admin password, writes `.env` (readable only by your user), and runs the end-to-end check.

### By hand (any OS)

```bash
export TGV_BOT_TOKENS="123:AAA...,456:BBB..."
./target/release/tgvault telegram-chats     # prints the channel id, like -1001234567890
./target/release/tgvault gen-secrets        # prints TGV_MASTER_KEYS and TGV_SERVER_SECRET
# put everything into .env (see .env.example), then:
set -a; . ./.env; set +a
./target/release/tgvault telegram-check
```

Expected result:

```text
ok   chat -1001234567890: 'my-vault' (channel)
ok   @my_vault_bot: administrator with the needed rights
ok   @my_vault_bot: upload, download and delete work (812 ms)
Telegram setup is ready.
```

A `FAIL` line says exactly what is missing, for example `missing Delete messages`.

If `telegram-chats` prints nothing, the bot has not seen the channel yet: make sure it is an administrator, post a new message in the channel, and run it again. You can also forward a channel post to `@userinfobot` to read the id.

## 4. Run

```bash
./target/release/tgvault          # panel on http://127.0.0.1:8787
```

On Windows, `.\scripts\run.ps1` loads `.env` and starts the server.

## Moving data in from another server

tgvault can download a URL itself and stream it, encrypted, into Telegram. See [IMPORT-API.md](IMPORT-API.md).

```bash
curl -H "Authorization: Bearer $TGV_KEY" -H "Content-Type: application/json" \
  -d '{"url":"https://media.example.com/ep01.mp4","key":"series/ep01.mp4","wait":true}' \
  http://127.0.0.1:8787/api/v1/buckets/videos/import
```

## Keep these safe

- `TGV_MASTER_KEYS`: lose it and the stored data cannot be decrypted. Keep a copy in a password manager.
- `TGV_SERVER_SECRET`: changing it invalidates every API key and link.
- Bot tokens: if one leaks, run `/revoke` in BotFather, update `TGV_BOT_TOKENS`, and restart.

## Recovering after losing the server

With your bot token(s) and master key(s) you can rebuild the file index from the sealed backup pinned in the channel:

```bash
./target/release/tgvault restore data/restored.db     # then stop the server and replace data/tgvault.db
```
