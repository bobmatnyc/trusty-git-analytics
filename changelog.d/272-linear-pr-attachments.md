Added
- Linear issue attachments are collected (`attachments(first: 25)`, cap logged when exceeded) and stored in the new `linear_issue_attachments` table (migration v41). `ISSUE_FIELDS_VERSION` moves to 2, so the next `tga linear sync` re-reads every stored team once (about 597 issue-page calls for FLYR) (#272).
- New `pr_work_items` table (migration v41) links pull requests to work items from Linear PR attachments and from keys in the branch, PR title, PR body and the PR's commits. The correlation pass fills it; attachment URLs match `pull_requests.repository` case-insensitively and keep the stored spelling (#272).
- The PR title is a ticket-key source for commit correlation, ranked after the branch and before the PR body (#272).
