#!/bin/sh
set -eu
# This is a container-private namespace. No host firewall or Docker socket is used.
nft -f /etc/kyro/egress.nft
nft list table inet kyro_egress > /run/guard/loaded.nft
touch /run/guard/ready
# Keep the namespace alive after relinquishing the installer capability.
exec setpriv --bounding-set=-all --inh-caps=-all --ambient-caps=-all --no-new-privs sleep infinity
