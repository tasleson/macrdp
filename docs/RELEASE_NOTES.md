# Cutting a Release

Releases are tag-driven. Pushing a tag matching `v<major>.<minor>.<patch>` triggers the GitHub Actions workflow, which builds the macOS aarch64 binary and publishes a GitHub Release with a tarball attached.

## Steps

1. Make sure the `main` branch is in the state you want to release and all CI is green.

2. Choose a version following [Semantic Versioning](https://semver.org/). Increment:
   - **patch** (`v0.1.1`) for bug fixes
   - **minor** (`v0.2.0`) for new backwards-compatible features
   - **major** (`v1.0.0`) for breaking changes

3. Create and push the tag:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

4. The [Release workflow](../.github/workflows/release.yml) will run automatically. Monitor it at:

   ```
   https://github.com/<owner>/macrdp/actions
   ```

5. Once the workflow completes, a draft release is published at:

   ```
   https://github.com/<owner>/macrdp/releases
   ```

   Review the auto-generated release notes, edit as needed, then publish.

## Tarball contents

| Path | Description |
|------|-------------|
| `macrdp-server` | Server binary (aarch64-apple-darwin) |
| `packaging/launchd/com.macrdp.daemon.plist` | launchd service definition |
| `config.example.toml` | Annotated example configuration |

## If something goes wrong

Delete the tag locally and remotely, fix the issue, and re-tag:

```sh
git tag -d v0.1.0
git push origin :refs/tags/v0.1.0
# fix, commit, then re-tag
git tag v0.1.0
git push origin v0.1.0
```
