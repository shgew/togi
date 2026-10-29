use std/assert
use args.nu selection

const product = [togi togi-tuner]
const metadata = {
    workspace_members: [product tuner release cover facts],
    packages: [
        {id: product, name: togi}
        {id: tuner, name: togi-tuner}
        {id: release, name: togi-tool-release}
        {id: cover, name: togi-tool-cover}
        {id: facts, name: togi-facts}
        {id: dependency, name: serde}
    ]
}
const product_args = [-p togi -p togi-tuner]

def test-product-only [] {
    assert equal (selection $product $metadata []) $product_args
}

def test-tool-only-diff-retains-product [] {
    assert equal (selection $product $metadata [togi-tool-release]) ($product_args | append [-p togi-tool-release])
}

def test-mixed-diff-deduplicates-product-and-tools [] {
    assert equal (selection $product $metadata [togi-tuner togi-tool-release togi-tool-release]) ($product_args | append [-p togi-tool-release])
}

def test-multiple-tools-do-not-select-whole-concern [] {
    assert equal (selection $product $metadata [togi-tool-release togi-tool-cover]) ($product_args | append [-p togi-tool-release -p togi-tool-cover])
}

def test-tool-only-library-selects-own-tests-not-consumers [] {
    # Selecting facts runs its tests; compilation of its dependencies is not a
    # selection of their tests, nor of release/cover's consumer tests.
    assert equal (selection $product $metadata [togi-facts]) ($product_args | append [-p togi-facts])
}

def assert-rejected [name: string] {
    let message = (try { selection $product $metadata [$name]; null } catch {|err| $err.msg })
    assert equal $message $'coverage package is not a workspace member: ($name)'
}

def test-unknown-name-fails [] { assert-rejected missing-package }
def test-nonworkspace-dependency-fails [] { assert-rejected serde }
def test-option-is-not-a-package [] { assert-rejected '--workspace' }

def test-cli-preserves-package-spelling [] {
    let result = (^nu --no-config-file tools/cover/args.nu '-p togi -p togi-tuner' togi-tool-release togi-tool-release | complete)
    assert equal $result.exit_code 0
    assert equal ($result.stdout | str trim) '-p togi -p togi-tuner -p togi-tool-release'
}

def test-cli-rejects-before-producing-arguments [] {
    let result = (^nu --no-config-file tools/cover/args.nu '-p togi' serde | complete)
    assert ($result.exit_code != 0)
    assert equal $result.stdout ''
    assert str contains $result.stderr 'coverage package is not a workspace member: serde'
}

test-product-only
test-tool-only-diff-retains-product
test-mixed-diff-deduplicates-product-and-tools
test-multiple-tools-do-not-select-whole-concern
test-tool-only-library-selects-own-tests-not-consumers
test-unknown-name-fails
test-nonworkspace-dependency-fails
test-option-is-not-a-package
test-cli-preserves-package-spelling
test-cli-rejects-before-producing-arguments
print '10 coverage selection tests passed'
