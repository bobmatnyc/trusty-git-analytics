Added
- `notion.user_map` maps Notion user ids to emails, so Notion activity is attributed to the tga identity even when the token cannot list users. Each sync also attributes already-stored rows of a newly mapped id; unmapped ids stay unresolved and counted (#257).
