#!/bin/sh
rg '192.168.176.130' -l ~/.steam/steam/steamapps/common/The\ Jackbox\ Party\ * | parallel sh update_jackbox_sed.sh
