# Markdown summary of a Trivy JSON report, for the job summary (see action.yml).
# Builds fail on HIGH or CRITICAL vulnerabilities that have a fix.
[.Results[]?.Vulnerabilities[]?] as $v
| [$v[] | select((.Severity == "HIGH" or .Severity == "CRITICAL") and (.FixedVersion // "") != "")] as $bad
| "## Image scan (Trivy)",
  "",
  "\(.Metadata.OS.Family // "?") \(.Metadata.OS.Name // "") base image. Builds fail on HIGH or CRITICAL vulnerabilities that have a fix; all findings are in Security → Code scanning.",
  "",
  "| Severity | With a fix | All |",
  "| --- | --: | --: |",
  (["CRITICAL", "HIGH", "MEDIUM", "LOW", "UNKNOWN"][] as $s
    | "| \($s) | \([$v[] | select(.Severity == $s and (.FixedVersion // "") != "")] | length) | \([$v[] | select(.Severity == $s)] | length) |"),
  "",
  if ($bad | length) == 0 then "✅ Nothing that fails the build."
  else "❌ These fail the build:", "",
    "| Package | Vulnerability | Installed | Fixed in |",
    "| --- | --- | --- | --- |",
    ($bad[] | "| \(.PkgName) | [\(.VulnerabilityID)](\(.PrimaryURL // "")) | \(.InstalledVersion) | \(.FixedVersion) |")
  end
