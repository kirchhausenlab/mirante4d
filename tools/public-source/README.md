# Deterministic Public Root

`build_root.py` exports approved tracked blobs from an exact source commit into
an isolated SHA-1 Git object database. It uses Git plumbing to create blobs,
trees, the parentless root commit, `main`, and the lightweight
`foundation-public-root-v1` tag. Ambient Git identity, hooks, attributes,
autocrlf, signing, worktree modes, and global/system configuration do not
participate.

The path policy excludes the three private operator records and permits no
executable path. Run the builder twice with the same source revision and
`SOURCE_DATE_EPOCH`; both manifest `commit_oid` and `tree_oid` values must be
identical. `scan_root.py` then checks the root topology, modes, path boundary,
binary/archive allowlists, and non-public text classes.

The generated root repositories and manifests are private cutover evidence and
must stay outside this checkout. WP-04 pushes only the verified root commit and
lightweight tag.

`validate_disposition.py` closes the cross-record invariants that JSON Schema
cannot express for the retained pre-foundation verification disposition:
unique IDs, exact computed counts, a disjoint and sorted post-WP-02 set, and
valid deleted/retained/moved/rewritten replacement relationships.
