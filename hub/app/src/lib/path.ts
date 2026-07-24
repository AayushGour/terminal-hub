// Shell integration (spec: 2026-07-23-shell-integration-design.md §6): a
// small pure display-formatting helper for the `cwd` field surfaced in
// TileFrame's titlebar and SessionList's row. No filesystem/backend access,
// no `~` substitution -- literal path segments only, so it's trivially
// testable in isolation (no unit-test runner is wired up for this frontend
// -- see package.json -- so this is exercised via the Playwright dev-smoke
// checks like the rest of this file's sibling pure helpers, e.g.
// `store.ts`'s `boxesOverlap`/`findFreeSpot`, rather than a standalone
// test file).
//
// Examples (see also the VITE_MOCK fixtures in mock.ts):
//   truncatePath("")                                    -> ""
//   truncatePath("/tmp")                                -> "/tmp"
//   truncatePath("/Users/aayush/projects")               -> "/Users/aayush/projects"   (exactly 3 segments, shown as-is)
//   truncatePath("/Users/aayush/projects/terminal-hub")  -> ".../aayush/projects/terminal-hub"
//   truncatePath("/a/b/c/d/e")                          -> ".../c/d/e"

/** Last 3 `/`-separated path segments, `.../`-prefixed only when segments
 * were elided (i.e. the path had MORE than 3 segments to begin with). A
 * path with 3 or fewer segments is returned unchanged (no ellipsis, no
 * reconstruction from parts -- avoids e.g. dropping a leading `/` that
 * splitting+rejoining would lose). No `~`/home-directory substitution. */
export function truncatePath(cwd: string): string {
  const segments = cwd.split("/").filter((s) => s.length > 0);
  if (segments.length <= 3) return cwd;
  return ".../" + segments.slice(-3).join("/");
}

/** Format session label in compact form for inline labels (titlebars, list rows).
 * Converts "Hub" -> "H", "External" -> "E", and appends "-{id}".
 * Examples:
 *   formatSessionLabel("Hub", 6)       -> "H-6"
 *   formatSessionLabel("External", 3) -> "E-3"
 *   formatSessionLabel("", 1)          -> "#1"  (unknown origin, fallback to plain id)
 */
export function formatSessionLabel(origin: string, id: number): string {
  if (origin === "Hub") return `H-${id}`;
  if (origin === "External") return `E-${id}`;
  return `#${id}`;
}
