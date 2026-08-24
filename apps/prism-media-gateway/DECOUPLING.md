# ⚠ Prism → Railyard decoupling required

Prism was moved from videogen/apps into Railyard, but it currently imports the video product's code:

    from app.domains.gateway.media_catalog import ...
    from app.domains.routing.policy import ...
    from app.domains.gateway.media_gateway import ...

Those `app.*` modules live in **videogen/apps/api** and are NOT present in Railyard. Prism will not run here until decoupled. Options:
1. **Call api over HTTP** — Prism becomes a thin gateway that calls Menivor's api media endpoints (cleanest; matches the "Railyard consumed over network" boundary).
2. **Vendor the needed modules** — copy media_catalog / routing.policy / the upstream adapters into prism as first-party code (fuller independence, more duplication).
3. **Extract a shared media-adapter package** both api and prism depend on.

Recommendation: (1) for now — Prism as a network client of Menivor's media API — until a shared media-provider package is warranted.
