# Data

An environment's **Data** tab shows what's in its Convex database, **read-only**.

- **Tables** (left): every table in the project with how many documents it has; the ones with
  documents come first. *Search tables…* finds any word anywhere in the name. Up and down arrows
  move through the list.
- **Documents** (right): the selected table's, newest first, 50 per page. The pager counts pages
  from the table's total ("1–50 of 240") and going to a page that hasn't been read yet reads it.
  - **Search** across all columns, **Filter** opens the filter rules (contains, is, greater than…)
    and sorting; a click on a header also sorts (Shift adds another criterion). With a search,
    filters or sorting, the table works over what's been read, with its own pager.
  - **Columns** hides columns, **Density** changes row height and **Export** copies what you see as
    CSV.
  - Dates are recognized by their values and read as date and time. Fields that point to another
    table show it in the header (`userId → users`) and their ids as tags.
  - Numbers are aligned and show their average at the bottom. The **Colors** checkbox tints them by
    where each value falls between the column's lowest (red) and highest (green).
  - With a single row selected, the whole document shows as JSON beside the table, with long lines
    wrapped. **Double-click** a row to copy its `_id`.

By environment:

- **Local** (devbox and throwaways): the daemon reads with its admin key, which never leaves it;
  you see how many documents each table has.
- **Cloud dev**: read-only. If you saved its deploy key in the **Keys** tab, the daemon reads as it
  does locally (record counts, pages with a cursor). The key has to belong to the environment's
  deployment (otherwise it's rejected without connecting). Without a key, it reads with your Convex
  CLI session (`npx convex data`), which gives neither counts (the list shows "—") nor a cursor.
- **Production**: hidden by default. To look at real data safely, restore a production backup into
  a throwaway (the rehearsal does it) and open its Data tab.

## Data and change rules

**Settings › Data and changes** decides, per kind of environment (devbox, throwaway, cloud dev,
production):

- **Read data**: whether the Data tab and agents through the MCP can read its records (always
  read-only). Production comes turned off.
- **Changes**: how much recipes that change the environment need: what the recipe says, at least an
  approval, typing the environment's name, Touch ID, blocked, or — except in production — no
  approval.

Queries that only read don't ask for approval when reading is allowed. The daemon keeps these rules,
and only the app can change them: an agent through the MCP or the CLI doesn't have that permission.
