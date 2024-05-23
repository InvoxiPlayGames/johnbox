#!/bin/sh
rg '(ecast.jackboxgames.com|jackbox.tv|JACKBOX.TV)' -l ~/.steam/steam/steamapps/common/The\ Jackbox\ Party\ * | parallel sh update_jackbox_sed.sh
