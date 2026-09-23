Fixed

- The engagement template (and so `instructions::ENGAGEMENT_TEMPLATE` and every
  package `taudit distribute` writes) pinned `tga = "7.1.1"` and
  `trusty-search = "0.54.0"`, neither of which crates.io has ever served, so a
  fresh engagement could not install its pinned tools. The pins are now
  `tga = "7.1.0"` and `trusty-search = "0.54.2"`, the newest published release
  of each line. `scripts/check-engagement-pins.sh` now fails any pin that names
  an unpublished or yanked version, and `--refresh` rewrites stale pins.
