#!/bin/sh
set -eu
# Run only in the private preparation container, after an exact Docker export
# of the pinned public tools image. Consumers mount this volume read-only.
cd /tools-root
# A gofer may not create mount points inside a read-only root. Preparation is
# explicit and finishes before the volume is exposed read-only to a workload.
mkdir -p input output work candidate driver criteria
# The baseline image's WORKDIR created an empty directory. Refuse to remove
# anything containing source; a checkout must never enter this public root.
if test -e workspace; then rmdir workspace; fi
find . ! -name '.kyro-root-*' -printf '%y %m %U %G %p %l\n' | LC_ALL=C sort > .kyro-root-metadata.txt
find . -type f ! -name .kyro-root-files.sha256 -print0 | LC_ALL=C sort -z | xargs -0 sha256sum > .kyro-root-files.sha256
sha256sum .kyro-root-files.sha256
