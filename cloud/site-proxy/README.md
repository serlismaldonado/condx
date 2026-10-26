# condx.heskala.com

The Condx Cloud site (HTTP actions: the QR's `/pair` page, `/ingest`, `/files`) on our own domain:
a Vercel project with nothing but a rewrite to the deployment's site (`<deployment>.convex.site`). `vercel.json` is written by the script with the real deployment and stays out of git (`vercel.example.json` shows its shape).
The Convex API stays on `<deployment>.convex.cloud`. Design: `docs/design/cloud-site.md` (S0).

```bash
scripts/cloud-site-proxy.sh https://<deployment>.convex.site   # points the rewrite there and deploys
condxd cloud site https://condx.heskala.com                     # the daemon's QR and SDK endpoint use it
```
