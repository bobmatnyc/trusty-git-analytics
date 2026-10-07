Changed
- A `classification.repo_categories` key with a `/` and no `:` (e.g. `acme-mono/api`), an empty repository or prefix around a `:`, or two keys naming the same repository and prefix now fail `tga classify`, `tga eval` and `tga rules test` before any write. A key holding `:` is now read as `<repo>:<prefix>`, not as a repository name (#167).
