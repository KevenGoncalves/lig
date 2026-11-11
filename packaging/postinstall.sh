#!/bin/sh
# Shells are set up per user, so only point at `lig install`, on the first install.
case "$1" in
    configure) [ -z "$2" ] || exit 0 ;; # deb: $2 is the version being upgraded from
    1) ;;                               # rpm: first install
    *) exit 0 ;;
esac
echo "lig: run 'lig install' as your user to turn on the popup in your shell."
