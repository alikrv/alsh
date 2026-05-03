# ALSH-RS

ALSH is a custom Unix shell (and scripting language!) written in Rust, designed to provide a simple yet powerful command-line interface with features like job control, control flow constructs, and built-in commands, as well as run ALSH scripts.

_And it's blazingly fast!_


## Features

### Core Shell Features
- **Command Execution**: Execute external programs and shell builtins
- **Classic Shell Pipes** (`|`): Chain commands using the pipe operator for byte-stream passing
- **ALSH Pipelines** (`->`): Value-aware stream pipelines for functions and commands
- **Value Chains** (`chain {}`): Composable transformations with implicit value threading using `@`
- **Background Jobs**: Run commands in the background with `&`
- **Job Control**: Manage background and suspended jobs with `jobs`, `fg`, `bg`, and `disown`
- **Signal Handling**: Proper handling of SIGINT, SIGQUIT, SIGTSTP, etc.
- **Non-TTY Support**: Works with piped input when stdin is not a terminal
- **Standard Library**: `std::` prefixed primitives are always available, and `@stdlib` enables shorthand names like `print(...)`, `trim(...)`, and `readfile(...)`
- **Startup Config**: `alsh` automatically sources `~/.alshrc` when launched interactively
- **Aliases**: Define shell-style aliases in `~/.alshrc` using `@define name command...`
- **Prompt Customization**: `alsh` uses `PS1` like bash and defaults to `\u@\h:\w$ ` when unset
- **History Expansion**: Use `!!` to rerun the last command
- **Timing**: Use `time <command>` to print a bash-style runtime summary

### Scripting & Language Features

#### Variables & Scope
- **Variable Declaration**: `let x = expression`
- **Global Variables**: `!global let author = "value"` for cross-scope and cross-file access
- **Lexical Scoping**: Block-based scope with shadowing support
- **Dynamic Typing**: Variables can hold multiple types (int, string, array, bool, struct, enum)
- **String Interpolation**: Double quotes with `$variable` substitution
- **Block Expressions**: `{ statements }` captures stdout and returns a string

#### Types & Data Structures
- **Primitives**: `int`, `string`, `bool`
- **Arrays**: `[1, 2, 3]` or `["a", "b"]`
- **Structs** (v1.1): Named records with typed fields
- **Enums** (v1.1): Closed sets of named constants for type-safe branching
- **Floats** (v1.1): Floating-point numeric type

#### Control Flow
- **Conditional**: `if`, `elif`, `else`
- **Loops**: 
  - `loop { }` - infinite loop
  - `loop count N { }` - loop N times
  - `loop interval N { }` - loop every N seconds
- **While Loops**: `while (condition) { }`
- **For Loops**: C-style `for (init; condition; update) { }`
- **Foreach**: `foreach item in array { }`
- **Break/Continue**: Loop control statements
- **Try/Catch**: Error handling with `try { } catch { }`
- **Scan** (v1.1): Enum-based pattern matching with `scan of EnumType { member: ... }`
- **Switch** (v1.1): Label-based branching with fallthrough support

#### Functions & C Integration
- **Function Definition**: `function name(param1, param2) { ... }`
- **Return Statements**: `return expression`
- **C Function Calls**: Direct access to C functions via `c::function_name(...)` syntax
- **Standard Library Functions**: 60+ built-in functions for strings, arrays, math, paths, and system operations

#### Three Execution Call Syntaxes
1. **Shell-style**: `echo "hello"` - executes external command
2. **ALSH Function**: `say("hello")` - calls user-defined or stdlib function
3. **C Function**: `c::puts("hello")` - calls C library function

### Preprocessing & Compilation
- **@define**: Text substitution for command-line aliases
- **@include**: Raw text file inclusion
- **@import**: Module loading with function/global access (no duplication)
- **@main**: Mark function as entry point
- **@justrunit**: Allow top-level command execution
- **@justcarryon**: Continue execution on errors instead of aborting
- **@stdlib**: Enable standard library shorthand (omit `std::` prefix)
- **@noffi**: Disable all `c::` function calls

### Filesystem & System
- **File I/O**: `std::readfile()`, `std::writefile()`, `std::appendfile()`
- **Path Operations**: `std::basename()`, `std::dirname()`, `std::joinpath()`
- **Directory Operations**: `std::listdir()`, `std::mkdir()`, `std::exists()`, `std::isdir()`, `std::isfile()`
- **Process Info**: `std::pid()`, `std::getuser()`, `std::which()`, `std::env()`

### String Utilities
- **Text Processing**: `std::upper()`, `std::lower()`, `std::trim()`, `std::strip()`
- **Pattern Matching**: `std::contains()`, `std::startswith()`, `std::endswith()`
- **Splitting & Joining**: `std::split()`, `std::join()`, `std::lines()`
- **Padding**: `std::padleft()`, `std::padright()`
- **Other**: `std::replace()`, `std::repeat()`, `std::strlen()`

### Array & Math Operations
- **Array Functions**: `std::len()`, `std::push()`, `std::pop()`, `std::first()`, `std::last()`, `std::slice()`, `std::reverse()`
- **Math**: `std::min()`, `std::max()`, `std::clamp()`, `std::even()`, `std::odd()`
- **Formatting**: `std::format_gb()`, `std::format_gib()`, `std::sizeof()`

### Installation & Deployment
- **Optimized Build**: `make build` or `cargo build --release`
- **System Installation**: `sudo make install` (copies pre-built binary, no cargo required)
- **Portable**: Single binary, no runtime dependencies

---

For detailed language specification and advanced usage, see [ALSHSPEC.md](ALSHSPEC.md).

