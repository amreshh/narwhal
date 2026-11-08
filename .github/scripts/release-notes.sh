#!/usr/bin/env bash
# Release notes: the commits of the pull requests merged since the previous
# release, grouped by area, as markdown on stdout. The release job in ci.yaml
# puts them above GitHub's generated notes, which list only the pull
# requests (one per release, as dev is squash-merged into main).
#
#   .github/scripts/release-notes.sh <previous release tag> <release commit>
#
# The areas are the labels in .github/labeler.yaml: a commit goes under the
# first one whose files it touches (release-notes.jq). Needs the git history
# back to the previous release, gh (with GH_TOKEN) and yq.
set -euo pipefail

previous=$1
target=$2
repo=${GITHUB_REPOSITORY:?}
here=$(dirname "$0")

# Each commit on main since the previous release stands for the commits of
# the pull request it merged, or for itself when it was pushed directly.
# Merge commits (e.g. main merged into dev) are left out.
shas=()
while read -r commit; do
  prs=$(gh api "repos/$repo/commits/$commit/pulls" --jq '.[] | select(.merged_at != null) | .number')
  if [ -z "$prs" ]; then
    shas+=("$commit")
  fi
  for pr in $prs; do
    while read -r sha; do
      shas+=("$sha")
    done < <(gh api --paginate "repos/$repo/pulls/$pr/commits" --jq '.[] | select((.parents | length) == 1) | .sha')
  done
done < <(git rev-list --reverse "$previous..$target")

# Each commit once, in order, with its subject and the files it changed.
commits=$(
  printf '%s\n' "${shas[@]}" | awk 'NF && !seen[$0]++' | while read -r sha; do
    gh api "repos/$repo/commits/$sha" --jq '{sha, subject: (.commit.message | split("\n")[0]), files: [.files[].filename]}'
  done | jq -s .
)

jq -r -n -f "$here/release-notes.jq" \
  --argjson commits "$commits" \
  --argjson labels "$(yq -o=json "$here/../labeler.yaml")" \
  --arg url "${GITHUB_SERVER_URL:-https://github.com}/$repo/commit"
