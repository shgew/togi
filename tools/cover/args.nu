# Product arguments come only from just _product-test-args; metadata owns names.
export def selection [product: list<string>, metadata: record, extra: list<string>]: nothing -> list<string> {
    let members = ($metadata.packages | where {|package| $package.id in $metadata.workspace_members } | get name)
    for name in $extra {
        if $name not-in $members {
            error make {msg: $'coverage package is not a workspace member: ($name)'}
        }
    }
    $product | append $extra | uniq | each {|name| ['-p' $name] } | flatten
}

# Standard input is `cargo metadata --locked --format-version 1 --no-deps` JSON.
def main [product_args: string, ...extra: string] {
    let metadata = ($in | from json)
    let product = ($product_args | split row ' ' | where {|arg| $arg != '-p' and $arg != '' })
    selection $product $metadata $extra | str join ' ' | print
}
