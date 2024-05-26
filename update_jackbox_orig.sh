#!/bin/sh
find "$1" -type f -name "jbg.config.jet" | parallel sh update_jackbox_sed_orig.sh
