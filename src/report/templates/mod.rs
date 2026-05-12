//! Embedded Tera template strings.
//!
//! Templates are embedded as `const &str` rather than loaded from disk so
//! the report formatters work in any deployment without external files.

/// Markdown summary template. See [`crate::report::formatters::markdown`].
pub const MARKDOWN_REPORT: &str = r#"# Git Activity Report

Generated: {{ generated_at }}
{% if period_start and period_end %}Period: {{ period_start }} – {{ period_end }}
{% endif %}
## Summary

- Total commits: {{ total_commits }}
- Total authors: {{ total_authors }}

## Top Authors

| Author | Commits | Insertions | Deletions |
|--------|---------|------------|-----------|
{% for a in top_authors -%}
| {{ a.name }} | {{ a.commit_count }} | {{ a.insertions }} | {{ a.deletions }} |
{% endfor %}

## Category Breakdown

{% for entry in category_breakdown -%}
- {{ entry.0 }}: {{ entry.1 }}
{% endfor %}

## Repositories

| Repository | Commits | Authors | Insertions | Deletions |
|------------|---------|---------|------------|-----------|
{% for r in repositories -%}
| {{ r.name }} | {{ r.commit_count }} | {{ r.author_count }} | {{ r.insertions }} | {{ r.deletions }} |
{% endfor %}
"#;
