#!/bin/sh
sed -Ei 's/(ecast.jackboxgames.com|jackbox.tv|JACKBOX.TV)/192.168.1.44/g' "$@"
