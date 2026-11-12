Fixed
- Notion collect no longer fails the whole sync when one page's comments answer HTTP 403 or 404 (#252). That page's comments are skipped, the run records one non-fatal fault counting such pages and warns about a likely missing "read comments" capability, and pages, users and the cursor are still stored. Notion errors now carry Notion's error `code` and `message`.
