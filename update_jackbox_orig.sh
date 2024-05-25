#!/bin/sh
find ~/.steam/steam/steamapps/common/ -type f -name "jbg.config.jet" | parallel sh update_jackbox_sed_orig.sh
