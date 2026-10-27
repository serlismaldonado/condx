# Condx Cloud and the cloud site (optional)

Condx works on the Mac alone. Two optional pieces take it further:

| Level | What you get | What it costs |
| --- | --- | --- |
| **Mac only** | Everything in the desktop app | Nothing to set up |
| **+ Condx Cloud** | The [phone app](15-phone.md), Claude from anywhere (remote MCP), state that stays visible with the Mac asleep | A Convex project of yours; the free tier is enough for one person |
| **+ Cloud site on boat** | Agents keep answering, fixing and deploying to cloud dev with the Mac off | A [boat](https://boat.dev) sandbox: the small one fits the 20 USD/month minimum |

**Production never leaves the Mac.** Promote and roll back only happen there, with Touch ID.

## Condx Cloud

Condx Cloud is a small Convex app (`cloud/` in this repo) that runs **in your own Convex
account**. The Mac, the phone and the cloud site sync through it. Nobody else hosts your data.

### Set it up

1. In the [Convex dashboard](https://dashboard.convex.dev), create a project (for example
   `condx-cloud`).
2. In that project: **Settings → Deploy keys → Generate production deploy key**.
3. From a clone of this repo (needs `bun` and Node.js), run:

   ```bash
   scripts/cloud-deploy.sh
   ```

   It asks for the key without showing it, deploys `cloud/` and links this Mac's daemon. The key is
   used once and not stored.
4. Pair your phone: [15-phone.md](15-phone.md).

Check it with `~/.condx/daemon/bin/condxd cloud status`.

### Your own domain (optional)

The QR and the pairing page use `<deployment>.convex.site` by default. To show your own domain
instead (for example `condx.example.com`), put a small Vercel project in front of it:

```bash
CONDX_SITE_DOMAIN=condx.example.com scripts/cloud-site-proxy.sh https://<deployment>.convex.site
~/.condx/daemon/bin/condxd cloud site https://condx.example.com
```

Then add the CNAME that Vercel shows at your DNS provider.

### Updating it

When a new Condx version changes `cloud/`, redeploy without re-pairing anything. Store the deploy
key once:

```bash
~/.condx/daemon/bin/condxd secret set condx-cloud-key
```

From then on, the **deploy-condx-cloud** recipe deploys it. It waits for your approval in the app
or on the phone. By hand: `CONDX_DEPLOY_ONLY=1 scripts/cloud-deploy.sh`.

## The cloud site on boat

The cloud site is the same daemon (`condxd`) built for Linux, running in a
[boat](https://boat.dev) sandbox. When the Mac is away (no heartbeat for 3 minutes, or it was shut
down), it answers the phone's messages, runs the agents and deploys to **cloud dev** and **Vercel
previews**. It has no production environments at all.

When the Mac comes back, its threads return to it, together with what the cloud site did
meanwhile.

### Set it up

1. In boat, create a sandbox. `small` (2 vCPU, 4 GB) is enough.
2. Inside it (SSH or `boat exec`), build and start the site:

   ```bash
   git clone https://github.com/serlismaldonado/condx.git && cd condx
   docker build -f deploy/site/Dockerfile -t condx-site .
   docker run -d --name condx-site --restart unless-stopped --network host -v condx-site:/data condx-site
   ```

3. Pair it like a phone. On the Mac, open **Settings → Condx Cloud → Pair a phone**, copy the QR's
   link, and inside the sandbox run:

   ```bash
   docker exec condx-site condxd site pair "<the QR's link>"
   ```

   Confirm it on the Mac: it shows up under **Cloud site**, apart from phones. The Mac then sends it
   your agent settings and provider keys, sealed so only the site can open them.

4. Let it clone your repos. Use a GitHub fine-grained token for the repo's owner, with contents and
   pull requests: write. It pushes `condx/*` branches and opens PRs; it never touches `main`.

   ```bash
   docker exec -i condx-site condxd secret set github-token:<owner>
   ```

   For agents on **Claude Code**: run `claude setup-token` on your Mac, and paste it in the agent
   settings: edit the **Claude Code** provider, field **Token (optional)**. It reaches the site encrypted. The site
   has no Claude Code login of its own.

### What it gets (least privilege)

| It gets | It never gets |
| --- | --- |
| Cloud dev deploy keys, provider keys, the GitHub token you give it | Production deploy keys |
| Your agent settings (default model, daily budget, agents at once) | Your Mac's Keychain |
| The phones the Mac trusts, to check their signatures | The right to approve: approvals stay on your phone or the Mac |

To remove it: on the Mac, **Remove** next to it under **Cloud site**. Inside the sandbox,
`docker exec condx-site condxd site unpair`.
