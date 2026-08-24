# rit — rust git

A from-scratch, **library-first** Rust implementation of the Git **loose-object model** —
the `git` equivalent. Byte-for-byte compatible with real Git (interop-tested against `git` 2.50).

- Object model: loose blobs/trees/commits/tags (read + write), git-exact SHA-1, canonical framing.
- ODB (zlib, atomic writes, SHA-1 verification on read), refs (HEAD/loose/packed/symbolic).
- `rit` CLI: `hash-object`, `cat-file`, `ls-tree`, `log`.
- 35 tests incl. real-git interop (our OIDs == `git hash-object`; `git fsck` accepts what we write).

The collaboration **forge** built on this core lives in [`../rithub`](../rithub) (the GitHub equivalent).

Deferred per the design gate: packfiles/delta, index/worktree, wire protocol v2, merge/rebase, SHA-256.
