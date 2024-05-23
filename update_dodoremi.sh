find "$1/The Jackbox Party Pack 10/games/NopusOpus/instruments/" -type d | parallel sh update_dodoremi_impl.sh {} $2
