use mod.nu [board-output run-gh]

def main [] {
    try {
        print --no-newline (board-output {|args| run-gh $args} (date now))
    } catch {|err|
        print --stderr $'board: ($err.msg)'
        exit 1
    }
}
