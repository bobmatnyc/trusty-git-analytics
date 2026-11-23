Fixed
- Serializing a config no longer writes `notion.user_map` Notion user ids and emails; it writes `user_map_entries` with the count, as `Debug` does. Loading the map is unchanged (#288).
