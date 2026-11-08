# Markdown summary of a Trivy JSON report, for the job summary and the pull
# request comment (see action.yml). Lists every vulnerability, with or without
# a fix. Builds fail on HIGH or CRITICAL ones that have a fix, unless a
# maintainer bypassed them in .trivyignore.yaml: Trivy (with --show-suppressed)
# reports those as modified findings, with the statement as the reason.
# $url: the findings in Security → Code scanning.

# Text that is safe inside a markdown table cell.
def cell: tostring | gsub("[\r\n]+"; " ") | gsub("\\|"; "\\|") | gsub("<"; "&lt;") | gsub(">"; "&gt;");

def severities: ["CRITICAL", "HIGH", "MEDIUM", "LOW", "UNKNOWN"];
def fixed: (.FixedVersion // "") != "";
def fails: (.Severity == "HIGH" or .Severity == "CRITICAL") and fixed and (.bypassed | not);

# The list stops here (most severe first); code scanning has them all.
def max_rows: 150;

def link: if (.PrimaryURL // "") != "" then "[\(.VulnerabilityID | cell)](\(.PrimaryURL))" else (.VulnerabilityID | cell) end;

( [.Results[]?.Vulnerabilities[]?]
  + [.Results[]?.ExperimentalModifiedFindings[]? | select(.Type == "vulnerability")
     | .Finding + {
         bypassed: true,
         reason: ((.Statement // "") | if . == "" then "no reason given" else . end),
         how: (if .Status == "ignored" then "bypassed" else .Status end)
       }]
) as $all
| [$all[] | select(.bypassed | not)] as $open
| [$all[] | select(fails)] as $bad
| ($all | sort_by([
    (.Severity as $s | severities | index($s) // 5),
    (if fails then 0 elif .bypassed then 1 elif fixed then 2 else 3 end),
    .PkgName, .VulnerabilityID
  ])) as $sorted
| "## Image scan (Trivy)",
  "",
  (if .Metadata.OS then "\(.Metadata.OS.Family) \(.Metadata.OS.Name // "") base image." else "No operating system in the image (built from scratch)." end)
    + " Every vulnerability is listed, with or without a fix. Builds fail on HIGH or CRITICAL ones that have a fix, unless a maintainer bypasses them in `.trivyignore.yaml`.",
  "",
  "| Severity | Fix available | No fix yet | Bypassed |",
  "| --- | --: | --: | --: |",
  (severities[] as $s
    | "| \($s) | \([$open[] | select(.Severity == $s and fixed)] | length) | \([$open[] | select(.Severity == $s and (fixed | not))] | length) | \([$all[] | select(.Severity == $s and .bypassed)] | length) |"),
  "",
  if ($bad | length) == 0 then "✅ Nothing fails the build."
  else "❌ These fail the build. Update the package (usually through a newer base image), or bypass them in `.trivyignore.yaml` with a reason:", "",
    "| Package | Vulnerability | Installed | Fixed in |",
    "| --- | --- | --- | --- |",
    ($bad | sort_by(.Severity != "CRITICAL", .PkgName, .VulnerabilityID)[]
      | "| \(.PkgName | cell) | \(link) | \(.InstalledVersion | cell) | \(.FixedVersion | cell) |")
  end,
  if ($all | length) == 0 then empty
  else "",
    # Open when it's short enough to read at a glance.
    "<details\(if ($all | length) <= 25 then " open" else "" end)><summary>All \($all | length) vulnerabilities</summary>",
    "",
    "| Severity | Package | Vulnerability | Installed | Fix | Status |",
    "| --- | --- | --- | --- | --- | --- |",
    ($sorted[:max_rows][]
      | "| \(.Severity | cell) | \(.PkgName | cell) | \(link) | \(.InstalledVersion | cell) | "
        + (if fixed then "✅ \(.FixedVersion | cell)" else "no fix yet" end) + " | "
        + (if .bypassed then "⏭️ \(.how | cell): \(.reason | cell)" elif fails then "❌ fails the build" else "" end) + " |"),
    (if ($all | length) > max_rows then "", "…and \(($all | length) - max_rows) more, less severe: see code scanning." else empty end),
    "",
    "</details>",
    "",
    "Details of each finding: [Security → Code scanning](\($url))"
  end
