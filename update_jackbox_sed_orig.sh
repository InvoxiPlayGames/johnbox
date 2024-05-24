#!/bin/sh
sed -Ei 's/(blobcast.jackboxgames.com|ecast.jackboxgames.com|jackbox.tv|JACKBOX.TV)/192.168.1.10/g' "$@"
