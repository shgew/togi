## Review record

**success** on aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.

| Source | Priority | Finding | Outcome |
|---|---|---|---|
| coordinator | P2 | Boundary untested (`a.go:5`) | fixed in dddddddddddddddddddddddddddddddddddddddd |

<details>
<summary>Review data</summary>

- Base: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
- Files: `a.go`
- Reviewers: coordinator: `a.go`
- Coverage: 1 uncovered range. Judged a boundary a consumer could see; became a finding.

</details>

<!-- togi-review {"version":3,"repository":"shgew/togi","pull_request":700,"head_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","base_sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","previous":null,"excluded_files":[],"files":["a.go"],"reviewers":[{"name":"coordinator","model":null,"files":["a.go"]}],"findings":[{"source":"coordinator","priority":"P2","finding":"Boundary untested","location":"a.go:5","url":null,"outcome":{"status":"fixed","sha":"dddddddddddddddddddddddddddddddddddddddd"}}],"coverage":{"uncovered":1,"judgment":"Judged a boundary a consumer could see; became a finding."},"verdict":"success"} -->
