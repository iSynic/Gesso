# start

How to design on this canvas: the tools, the workflow, and how to recover from errors. Read this first.

## The tools

| Tool | Use it to |
|---|---|
| `get_state` | See the open document: its node count, pages (with their top-level node counts), the current page, the selection, the viewport, undo/redo labels. Call it first. |
| `execute` | Change or read the document with a JavaScript program. One call is one undoable step. |
| `screenshot` | Look at your work: a PNG of some nodes (`ids`) or of the whole page. |
| `read_guide` | Read these guides: `start`, `api`, `nodes`, `layout`, `design`, `recipes`. |
| `validate` | Check a `.canvas` document (or the open one) against the schema. |
| `open`, `save` | Open or save a `.canvas` document folder (paths must be inside the allowed folders). |

If the desktop app is running, your edits appear in its window live and the user can undo them
with Ctrl+Z. Otherwise you work on a headless copy (`get_state` says `"mode": "headless"` or
`"app"`); remember to `save` it.

### From the command line

Without MCP, the `canvas` program does the same work, one script per run:

- `canvas run script.js -o out.canvas --screenshot page.png` runs a script on a new document,
  saves it, and writes a PNG of the whole page (with a 40 px margin of canvas around the
  content). `canvas run out.canvas next.js --save` continues from a saved document.
- Images the script takes with `screenshot(...)` are written next to the script as
  `<script>-1.png`, `<script>-2.png`, … (overwritten on the next run).
- A failed script exits with code 1 and prints the error JSON; fix the file and run again
  (`retry` with `edits` exists only over MCP).
- `canvas validate out.canvas`, `canvas render out.canvas --node ID -o node.png`,
  `canvas state out.canvas`, `canvas guide NAME`.
- Files a script exports must be in the current folder, the document's folder or a `--root`.

## The workflow

1. `get_state`. Find where things are.
2. Read before you write: `get({type: "frame"}, {fields: ["name", "bounds"]})` lists the
   top-level frames with their boxes. Don't guess ids or positions.
3. Make a change with `execute`. Build a whole screen or component per call, as a tree: one
   `insert` with `children` is faster and clearer than many small inserts.
4. Check it: call `screenshot` (or `screenshot(id)` inside the script) and look at the image.
5. Fix what's wrong with `update`, then look again.

A script is a plain JavaScript program (no `import`, no network, no files). The document API
(`insert`, `update`, `get`, `find`, …) is global. The value of the last expression is returned
to you, so end with what you want to see:

```js
// label: Add a heading
const h = insert(null, { type: "text", name: "Heading", text: "Welcome back", style: { fontSize: 32, fontWeight: 700 } });
get(h, { fields: ["name", "bounds"] })
```

The `// label: …` comment on the first line names the undo step ("Undo Add a heading").

## Units and coordinates

- Units are pixels. `x`/`y` are relative to the parent's top-left corner; `bounds` (from `get`)
  are page coordinates.
- Inside an auto-layout frame the layout places children: their `x`/`y` are computed for you.
- Put each new screen in free space: `findEmptySpace(width, height)` returns a spot to the right
  of the existing content.

## Errors

When a script throws, **nothing it did is kept**: the document and the undo history are as they
were. The error says what and where:

```json
{"error": {"message": "update: unknown property \"fil\" for a rectangle node", "line": 3, "column": 1,
           "sourceLine": "update(card, { fil: [\"#fff\"] })", "scriptId": "s4"}}
```

Fix it and send the whole script again, or send only the fix:

```json
{"retry": "s4", "edits": [{"from": 3, "text": "update(card, { fills: [\"#fff\"] })"}]}
```

`edits` replace lines `from`..`to` (inclusive, 1-based, `to` defaults to `from`) of the failed
script. Errors from the document name the property path, e.g. `node.children[2]: unknown
property "colour"`; the `nodes` guide lists every property.

Use `try`/`catch` inside a script when a step may fail and you want to carry on; anything caught
doesn't roll back the rest.

## Limits

- A script may use 100 ms of CPU by default; pass `timeoutMs` (up to 10000) for big jobs.
- 64 MB of memory. `print(...)` output comes back in `logs`.
- Ids are 22-character strings. Use the ids that calls return; never invent one.

## Next

- `api`: every function, with parameters and return values.
- `nodes`: node types and all their properties.
- `layout`: auto layout, sizing (`fixed`, `hug`, `fill`) and grids.
- `design`: spacing, type, colour and naming that make screens look professional.
- `recipes`: complete examples (card, form, list, nav bar, dashboard) to adapt.
