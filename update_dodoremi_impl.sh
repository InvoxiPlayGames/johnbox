dirname=$(basename "$1")
cat "$1"/config.json | jq --raw-output '.chains.[].nodes.[].options.urls | values | flatten | join("\n")' | parallel curl "https://$2/@cdn.jackboxgames.com/nopus-opus/instruments/$dirname/{}.ogg" -o /dev/null
