# The markdown of release-notes.sh: $commits ([{sha, subject, files}]) under
# a heading per area. $labels: .github/labeler.yaml as JSON, whose order is
# the areas' priority. $url: where commit links point (…/commit).

# Headings for the labels; any other label is used as it is.
def heading:
  {security: "Security", image: "Container image", server: "Server",
   frontend: "Frontend", office: "Office documents", shared: "Shared types",
   deploy: "Kubernetes", ci: "CI", fuzz: "Fuzzing",
   dependencies: "Dependencies", docs: "Docs", other: "Other"}[.] // .;

# A labeler glob as a regex: ** spans folders, * and ? stay within one, and
# names starting with a dot match too (labeler's dot: true).
def glob_re:
  "^" + (gsub("(?<c>[.+^$(){}|\\[\\]\\\\])"; "\\\(.c)")
    | gsub("\\*\\*/"; "\u0001") | gsub("\\*\\*"; "\u0002")
    | gsub("\\*"; "[^/]*") | gsub("\\?"; "[^/]")
    | gsub("\u0001"; "(.*/)?") | gsub("\u0002"; ".*")) + "$";

# Each label's globs as regexes, in the file's order.
[$labels | to_entries[]
  | {label: .key, res: [.value[] | .["changed-files"][]? | .[] | (arrays[], strings) | glob_re]}
] as $areas

# The first area any of the files belongs to.
| def area: . as $files
    | first($areas[] | select(any(.res[] as $re | $files[] | test($re); .)) | .label) // "other";

[$commits[] | . + {area: (.files | area)}] as $listed
| if ($listed | length) == 0 then empty
  else
    "## Changes",
    (([$areas[].label] + ["other"])[] as $a
      | [$listed[] | select(.area == $a)]
      | select(length > 0)
      | "", "### \($a | heading)", "",
        (.[] | "- \(.subject) ([\(.sha[0:7])](\($url)/\(.sha)))"))
  end
