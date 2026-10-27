---
name: condx
description: Work with the developer's Convex projects (heskala-erp and others) through the Condx connector — from a phone or anywhere, with the Mac on or off. Use it whenever the developer mentions Condx, heskala-erp, trying or previewing a project, giving a task or feature to the agents, cloud dev, or asks for a URL to test.
---

# Working with Condx from anywhere

Condx runs the developer's Convex projects (heskala-erp is the default) on their Mac and, when the Mac
is off, on a cloud site. You reach it through this connector. The developer is often on a phone: what
they can open is a public URL, never `localhost`.

## Who answers

- **The Mac is on:** it answers. Its own services (`list_services` → `services`) only open on that Mac.
- **The Mac is off:** the cloud site answers. It works against each project's **cloud dev** (a shared
  Convex development deployment); there's no local backend.
- The cloud site's preview opens from anywhere either way: `list_services` → `cloudSite.services[].openUrl`
  (or `services[].publicUrl` when the cloud site answers).

## Try a project ("give me the URL to try it")

1. `list_services` with the project. If `cloudSite` (or the cloud site's services) has the frontend
   running with a URL, give that URL.
2. If it's not running: `service_action` name `all`, action `start`, the project and a `branch` (the one
   the developer is working on; without one, the cloud dev's branch, `dev` in heskala-erp). Then poll
   `list_services` until the frontend is running and give its URL. It takes a minute or two.
3. `get_vercel` is **production**. Don't offer it to try changes.

Starting the preview deploys that branch to the project's cloud dev: it replaces what cloud dev had.
Say so when you switch branches.

## Build a feature or fix ("do X in heskala-erp")

1. `give_task` with the project and a clear, self-contained description (what, where, what not to touch).
2. A flow runs on its own: an agent does it on its branch (`condx/fix-…`, pushed to GitHub), another
   reviews it, the project's checks run (lint, tests) and, on the cloud site, the branch runs on cloud
   dev with a preview. The developer gets a notice on the phone.
3. Follow it with `get_workflow_run` (each node's status) or `read_thread`. It takes several minutes;
   don't wait in a tight loop: check every 30-60 s, or tell the developer you'll check back.
4. When it's done, give the preview URL (`list_services`).
5. To iterate: `post_to_thread` in the task's thread with `@fixer` and what to change; it commits again
   on its branch. Then `service_action` restart `all` with `branch` = that branch, and give the URL.
6. If a node failed, `read_thread` / `get_workflow_run` say why; `retry_run` retries from there.

## Ask about the code

`start_chat` with the project and a question: a Condx agent reads the repo (on the Mac or the cloud
site) and answers; `read_thread` for the answer.

## Never

- Production: no tool here deploys to production or approves anything. Passing to production is merging
  `dev` into `main`, from the developer's Condx app.
- Keys and secrets: you can't read them and shouldn't ask for them.
- Approvals (deploys that need one) happen on the developer's phone or Mac, not here: say what's waiting.
