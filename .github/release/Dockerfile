# The release of an image that's already built (ci.yaml, on main: the image
# dev built from the same code), with the release's version and commit set
# for the app's About dialog. Only settings change: no new layers, nothing is
# rebuilt.
#
#   docker build --build-arg IMAGE=ghcr.io/owner/narwhal@sha256:… \
#     --build-arg NARWHAL_VERSION=1.2.3 --build-arg NARWHAL_COMMIT=… .github/release

ARG IMAGE
FROM ${IMAGE}
ARG NARWHAL_COMMIT
ARG NARWHAL_VERSION
ENV NARWHAL_COMMIT=${NARWHAL_COMMIT} \
    NARWHAL_VERSION=${NARWHAL_VERSION}
