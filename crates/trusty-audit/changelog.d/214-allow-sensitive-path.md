Fixed
- `taudit` approves a checkout under `/tmp`, `/private/tmp` or `/var/folders` with `trusty-search index add --allow-sensitive-path`, so a work dir in a scratch location is no longer refused at approval. Other checkouts are approved without the flag, as before ([#214](https://github.com/bobmatnyc/trusty-git-analytics/issues/214)).
