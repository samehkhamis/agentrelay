# Configuration

Profiles live in `app.toml`, one table per profile:

```toml
[dev]
region = "local"
replicas = 1

[prod]
region = "us-east"
replicas = 3
```

Back to [getting started](getting-started.md).
