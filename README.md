# ResearchWiki

Native desktop app that gathers academic articles from open sources (arXiv, PMC, PubMed, EuroPMC, medRxiv, bioRxiv, OpenAlex, Crossref, Unpaywall, Semantic Scholar, ClinicalTrials) and builds a navigable knowledge-graph wiki from them.

Single-binary Windows desktop app built on egui/eframe. Data lives at `%APPDATA%\ResearchWiki\`.

## Status

Early development. Forked from a server-mode predecessor; UI is being rebuilt as a native desktop application. No public release yet.

## Building from source

```
cargo build --release
```

Requires:
- Rust 1.83+
- Windows: Visual Studio Build Tools 2019+ with "Desktop development with C++" workload (for the bundled SQLite C source)

## License

MIT. See [LICENSE](LICENSE).
