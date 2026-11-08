<!--
Merging into main releases the next patch version. For a minor or major
release, see "Version on main" at the top of .github/workflows/ci.yaml.
-->

## What and why

<!-- What changes, and the problem it solves. "Fixes #123" closes an issue. -->

## Testing

<!-- What you checked beyond CI (which runs the tests, lint, security, fuzzing
and the image build and scan): commands, what you tried in the app. -->

## Checklist

- [ ] README and comments match the new behaviour
- [ ] Dockerfile changes: the image runs (`docker run`, then the app and `/healthz`)
- [ ] deploy/ changes: `kubectl kustomize deploy` renders
- [ ] Bypasses added to `.trivyignore.yaml` have a statement and an expiry date
