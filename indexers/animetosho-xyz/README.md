# AnimeTosho.net Indexer

An anime indexer backed by AnimeTosho.net's feed API. One plugin can return either Usenet NZBs through its Newznab endpoint or torrents through its Torznab endpoint.

## Configure in Scryer

**api_key** is required. **base_url** defaults to https://feed.animetosho.net. Set **download_mode** to **nzb** (the default; **usenet** is accepted) or **torrent** (**torznab** is also accepted). **additional_params** is appended to provider requests.

Saved URLs on `animetosho.xyz` and its subdomains are translated to the corresponding `.net` host inside the plugin, including URLs with paths or ports. Custom hosts are preserved; no host or database migration is required.

## Search behavior

The plugin supports recent, RSS, automatic, and interactive anime searches. Its request model is title, category, and limit based; it does not promise an external-ID lookup even though returned releases can carry TVDB, TMDB, and AniDB metadata.

Results are capped at 200 per page and ten pages, with a two-second rate-limit hint. In torrent mode it preserves tracker-style metadata including seeders, peers, leechers, info hashes, magnets, and volume factors. In NZB mode it returns the corresponding Usenet releases. Scryer chooses which normalized result to acquire.
