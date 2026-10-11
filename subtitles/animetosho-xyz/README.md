# AnimeTosho.net Subtitles

An anime episode subtitle catalog that searches AnimeTosho's JSON release API and retrieves subtitle attachments from the AnimeTosho site or storage service.

## Configure in Scryer

**api_key** is required. **base_url** defaults to https://feed.animetosho.net and **site_url** defaults to https://animetosho.net; both are required because the API supplies release data while the site supplies subtitle links. **search_threshold** controls how many releases are inspected per request, with a default of 6 and a hard range of 1–15.

The validation action performs a small API query, so it validates both the configured endpoint and key.

Saved URLs on `animetosho.xyz` and its subdomains are translated to the corresponding `.net` host inside the plugin, including URLs with paths or ports. Custom hosts are preserved; no host or database migration is required.

## Search and download behavior

This provider accepts episode requests in the anime facet only. It prefers an AniDB episode ID when Scryer has one, otherwise searches by title and filters attachments against requested languages. It does not support media-file hash lookup, forced/HI flags, or AI/machine-translation filters.

The provider returns files in the language set advertised by AnimeTosho, including English, Japanese, Chinese, and common European languages. Downloads follow at most four redirects across the declared AnimeTosho API, site, and storage hosts and retry bounded rate limits. Scryer decides whether a returned attachment should be installed.
