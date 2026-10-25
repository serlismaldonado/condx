# Checking status

## The menu bar icon

| Icon | Means |
| --- | --- |
| **Condx ✓** | All good |
| **Condx ⚠ N** | There are N things to look at (for example, a pending approval or a failed job) |
| **Condx ✕** | An environment isn't responding or the daemon is down |

Its menu shows one line per environment and has **Open Condx** and **Quit**.

## The window

A flat, compact style like Zed or Cursor: neutral greys, thin straight borders, no shadows, Zed's
typeface (IBM Plex Sans; JetBrains Mono for code and figures) and color only where it matters.

**Title bar** (integrated, with the macOS buttons):

- the **project selector** (you can also add a project from there);
- the repository's branch and commit and, in an environment, its name and kind (devbox, cloud dev,
  production);
- on the right, what's running and what's waiting for approval (click to go there), and the
  daemon's status.

**Sidebar:**

- **Built around the environments:** almost everything belongs to one (its services, its jobs, its
  workflows, the app tabs connected to it), so each environment is the way into its own things, in
  tabs.
- **The groups:**
  - **Overview.**
  - **Environments:** local, dev, prod… with their health. The number says how many services are up
    (on the devbox) or what's waiting for approval in that environment.
  - **Project:** Workflows (see [Workflows](12-workflows.md)), Git & CI (Git in other projects) and
    Terminal: they serve every environment.

  Each group opens or closes with its arrow. In a project without registered environments, the
  Environments group shows "Local environment".
- **Jobs and approvals of every environment:** from the notices in the title bar ("N jobs running",
  "N to approve").
- **At the bottom:** the **notifications** bell, the **theme** (system → dark → light), the
  **guide**, **settings** and the button to collapse the sidebar into an icon rail.
- **Arrow keys:** with the sidebar focused, up and down move between pages.

**Content:** no repeated title; the sidebar marks the page. Logs and lists with their own scroll
move their content first; at the very top or bottom, the wheel carries on with the page. The schema
diagram never moves the page.

**Brand color:** yellow **#F3B01C** replaces blue in everything that isn't a status: each view's
main button (start, request job, approve, create…), active toggles, the selected tab's underline,
the sidebar's active icons, the selected table's relations in the schema, links, selection and
switches. Text on yellow is dark (9.5:1 contrast). As text or a line on the background, dark mode
uses the same yellow (9:1); light mode a dark gold (#A16207, 4.9:1), because yellow on white doesn't
reach 2:1. Statuses (online, error, warning, info) keep their colors.

**Small screens:** with a narrow window (under 820 px) the sidebar folds into the icon rail on its
own (without changing the preference); under 1000 px the overview rows hide latency and lag. Figure
cards wrap into rows, search fields and dropdowns shrink, and the window doesn't go below 640 × 480.

**Preferences:** the app remembers the collapsed sidebar, closed groups, the project, the theme and
the language in `~/.condx/app/app.sqlite3`, on this Mac only.

## Notifications

They arrive at the bell:

- what needs attention (an environment not responding, CI in red…);
- jobs that finish or fail;
- what's waiting for your approval;
- an action's errors.

New ones also show up as a floating notice. The panel keeps the last 200: the **×** deletes one,
**Mark as read** clears the bell's number and **Clear** deletes them all. Closing the panel marks
what you saw as read.

## The pages

- **Overview:**
  - At the top, figures: environments online, jobs running, to approve, attention and unpushed
    commits.
  - Below, the **environments in rows**: health, kind, Convex latency, commit and lag. Click a row:
    its detail. The devbox row has the button to start or stop it on the right (the same as in its
    summary).
  - At the end, **folded**: the production frontend, CI, the deployer and the repository. Each
    title already says its status ("CI · 1 of 2 green"); open them for the detail.
- **An environment** (Environments group): one row at the top with the tabs on the left and, on the
  right, **Start all** and **Stop all** (in the devbox summary) or the link to the Convex dashboard.
  Tabs go from what you look at most to least, and each kind shows its own:

  | Tab | devbox | cloud dev | production |
  |---|---|---|---|
  | Summary | ✓ (with services) | ✓ | ✓ |
  | Workflows | ✓ | ✓ | ✓ |
  | Live (heskala-erp) | ✓ | ✓ | ✓ |
  | Logs, Jobs | ✓ | ✓ | ✓ |
  | Deploys | | ✓ | ✓ |
  | Data, Variables, Schema, Keys | ✓ | ✓ | ✓ |

  - **Summary:** on the devbox it starts with its **services** (this Mac's backend, deployer and
    frontend, with their CPU and memory; see [Local environment and terminal](10-local-terminal.md))
    and below, folded: **Logs** (open when you arrive), **Convex & deploy**, **Project** and **Local
    environment mode**. In cloud environments, only the Convex and deploy card.
  - **Start and stop:** with nothing up you only see **Start all**; with everything up, only **Stop
    all** (in red); halfway, **Start the rest** and **Stop all**.
  - **While starting or stopping**, everything turns **orange**: the button ("Starting…" /
    "Stopping…"), each service's badge, and the environment's dot in the sidebar and the overview.
    The change shows instantly (without waiting for the 3 s poll) and is checked every half second
    until the daemon confirms the final state.
  - **Variables:** names only, never values, in a table with search.
  - **Logs:** Convex functions with their time, function and message. Filter by **minimum level**
    (TRACE…ERROR) and by **search**, without fetching them again.
  - **Deploys:** the ones the daemon made, as a commit list with message and author.
  - **Data:** see [Data](13-data.md).
  - **Schema:** see below.
  - **Jobs:** request one with the recipes that run on that environment (without choosing it) and
    its list, with the selected one's detail. See [Requesting a job](03-jobs.md).
  - **Workflows:** the workflows that touch that environment and what's waiting for approval there,
    to approve it right there. See [Workflows](12-workflows.md).
  - **Keys:** the environment's deploy key, saved in the Keychain.
  - **Live:** the web app tabs connected to that environment's Convex URL.
- **Git & CI** (**Git** in other projects): like the environments, each section folds (Changes
  open when you arrive) and the title already says what matters:
  - **Changes · N:** files by section (unstaged, staged, untracked, conflicted), with their status
    letter and lines;
  - **Branches · N:** with their distance to the remote;
  - **Latest commits · N:** with their references;
  - in heskala-erp, **Submodules** ("1 up to date" or how many differ from the remote) and a **CI**
    per repo with its last run's status (click a run: it opens on GitHub).
- **Live** (each environment's tab): if several app tabs are connected, pick one from a dropdown.
  - Below, its figures, the Convex functions in a table sortable by any column, the errors and,
    folded, the full snapshot as a JSON tree with search.

## An environment's schema

An environment's **Schema** tab shows its Convex tables.

- **Tables panel:** the panel button (at the start of the bar) hides it to give the diagram the
  full width, and shows it again ("Tables"). It's remembered.
- **On the left**, the `convex/` folder tree, as in the repo. Each table hangs from the folder that
  `convex/schema.ts` imports it from; `@convex-dev/auth` tables go apart. It has search and each
  folder says how many tables it has. Click a table: it selects it. Click a folder: it switches to
  **Folder** mode with that folder.
- **On the right**, the diagram: each table is a box with its fields, and each reference is a line
  to the table it points to. Three modes:
  - **Focus** (default): the selected table in the center, the tables it points to on the right and
    the ones pointing to it on the left, in short columns.
  - **Folder:** the selected folder with its subfolders, as a tree (see Layout). All its references
    are drawn, including the ones between subfolders.
  - **All:** all of `convex/`. References inside each folder and the selected table's are drawn.
- **Layout** (in Folder and All):
  - **By folders:** a wide tree. Each folder has its main table (the one named like it, or else the
    most referenced from inside), with its title highlighted, on the left of its frame; to its right
    goes everything it has: its tables and its subfolders' blocks, which repeat the same thing. The
    right side wraps into rows so the whole is wider than tall. Dotted lines join the main table
    with its first column.
  - **By relations:** layers from left to right, each table to the right of the ones it points to,
    in the order with the fewest crossings; tables without relations go in a separate row below.
- **Lines:** at right angles and going around the boxes, with a dot where they enter. A box's line
  goes straight while you drag it; it's recalculated when you drop it.
- **Minimap:** bottom right, when not everything is visible: the boxes in small and a rectangle with
  what you see. Click or drag on it: the diagram goes there.
- **Select and move:** click anywhere on a box to select it; drag it to move it (its folder's frame
  follows) and the position is remembered. In Focus, selecting another table puts it in the center,
  so it's selected on release without having moved.
- **The selected table's relations:** its lines go in another color. With **Only the selected
  one's** (in Folder and All) the others are hidden.
- **Moving around:** two fingers on the trackpad (or the wheel), or dragging the background, move
  the diagram; pinch or **⌘ + wheel** zoom where the pointer is (from 3 % to 400 %). There are also
  **−**, **+** and **Fit**, which frames everything visible. The diagram doesn't move the page.
- **From afar:** text shrinks but doesn't disappear. Fields show until a row measures 6 px; after
  that, a line per field. The table name doesn't go below 9 px (if it doesn't fit in its strip it
  goes right above the box); very far away, each folder's main table and the selected one carry
  their name on a background. Folder names stay readable.
- **Full screen:** hides the sidebar and tabs and leaves the schema in the whole window. **Exit full
  screen** brings it back; so does changing page.
- **Height:** drag the diagram's bottom edge to make it taller or shorter (from 280 px). It doesn't
  go past the window, so the edge stays visible; when the window shrinks, the height is trimmed.
  It's remembered.
- **Spacing:** compact, normal or wide, to separate the boxes more.
- **Below**, the selected table: its fields, its folder, which tables it points to and its
  **indexes** (normal, search and vector) with their fields.
- **Remembered:** what you move, the zoom, the spacing and the height stay saved, per environment
  and per mode.
- **Where it comes from:** in local environments it's the **deployed** schema. In cloud (dev,
  production), the Convex CLI doesn't let you read the schema, so the project's local backend schema
  is shown, marked as "checkout schema"; it has to be running.
