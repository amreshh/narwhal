<!--
Merging into main releases the next patch version. For a minor or major
release, see "Version on main" at the top of .github/workflows/ci.yaml.
-->

## What and why

<!-- What changes, and the problem it solves. "Fixes #123" closes an issue. -->

## Testing

<!-- What you checked beyond CI (which runs the tests, lint, security, fuzzing,
dependency review, and builds and starts the image): commands, what you tried
in the app. -->

## Checklist

- [ ] README and comments match the new behaviour
- [ ] Dockerfile changes: the app works in a browser from the image (CI only checks that it starts and answers)
- [ ] deploy/ changes: `kubectl kustomize deploy` renders
- [ ] Bypasses added to `.github/dependency-review.yaml` say why, and when to look at them again
