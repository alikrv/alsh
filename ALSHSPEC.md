#  ALSH Language Specification

## 1. Lexical Structure

### 1.1 Identifiers
identifier ::= `[a-zA-Z_][a-zA-Z0-9_]*`

Examples:
- `x`
- `my_var`
- `_data123`

### 1.2 Keywords
Reserved words:

`let` `function` `fn` `return`
`if` `elif` `else`
`while` `loop` `for` `foreach` `in`
`break` `continue`
`try` `catch`
`struct` `enum`
`scan` `switch` `default`
`count` `times`
`endfunction` `endfn` `endif` `endwhile` `endfor` `endforeach` `endloop` `endtry`

Note: the `end*` tokens are legacy-style terminators and are supported mostly as an optional easter egg rather than required syntax.

#### Preprocessor:

- `@include`
- `@import`
- `@define`
- `@main`
- `@justrunit` 
- `@justcarryon` 
- `@stdlib`
- `@noffi`
- `!global`

### 1.3 Literals
- Strings
- `"hello world"`
- 'hello world'`
- Double quotes support interpolation: `$var`
- Single quotes are raw (no interpolation)
- Numbers
- 123
- -42
- 0


### 1.4 Variables
`$identifier`

Example:
- `echo $name`

### 1.5 Comments
- `# comment`
- `// comment`
- `#!` only valid on first line (shebang)


## 2. Preprocessor

Preprocessing happens before parsing.

### 2.1 `@define`
@define NAME value
- Pure textual substitution
- No scope
- No evaluation

### 2.2 `@include`
`@include "file.alsh`
- Raw text inclusion
- No isolation
- Order matters

### 2.3 @import
`@import "file.alsh"`
- Loads file as a module
- Makes available:
- functions
- !global variables
- Does NOT paste text
- Avoids duplication

### 2.4 Entry point
`@main`

Marks a function as entry point:
```
@main
function start() { ... }
```

`@justrunit`

Allows top-level execution:

`@justrunit`

### 2.5 `@justcarryon`
`@justcarryon`

When this directive is present, runtime errors during command execution or function calls
are reported but do not abort script execution. The script continues with the next statement.

### 2.6`@noffi`
`@noffi`

When this directive is present, any `c::` function call in the current file is disabled.
The call is removed during preprocessing so the C function is never invoked.

### 2.7 @stdlib`
`@stdlib`

Enables the ALSH standard library shorthand names in the current script.
Functions in the standard library are always available under the `std::` namespace, for example `std::println(...)`.
When `@stdlib` is present, the same functions may also be called without the `std::` prefix, for example `println(...)`.

### Execution rules:
- If @main exists → run that function
- Else if @justrunit → execute top-level
- Else → do nothing

## 3. Variables & Scope
### 3.1 Declaration
`let x = expression`

### 3.2 Global variables
`!global let author = "Alina"`
Accessible:
- everywhere in file
- in all child scopes
- in files that @import or @include it

### 3.3 Scope rules
- `{}` creates a new scope
- functions create a new scope
- variables are lexically scoped
- Visibility:
```
Location	Access
same scope	✅
inner scope	✅
outer scope	❌
```

### 3.4 Shadowing

Allowed:
```
let x = 10

{
    let x = 20
}
```
## 4. Types

Minimal v1 types:

- `int`
- `string`
- `array`
- `bool` (implicit via expressions)

### 4.1 Arrays
- `[1, 2, 3]`
- `["a", "b"]`

### 4.2 Structs (v1.1)
Structs are named records with typed fields.

Syntax:
```
struct person {
    age: int
    name: str
    height: float
}
```

- `int` remains the integer type.
- `float` is a new numeric subtype in v1.1.
- `str` is a v1.1 alias for `string`.
- Struct values are created with a literal record expression.
- Fields are accessed using dot notation.

Example:
```
let p = person { age: 30 name: "Alice" height: 1.70 }
echo $p.name
let h = $p.height
```

### 4.3 Enums (v1.1)
Enums define a closed set of named values.

Syntax:
```
enum bread_id {
    brioche
    fougasse
    baguette
    croissant
}
```

- Enum members are typed constants.
- Use `bread_id.brioche` to refer to a member.
- Enum values compare by identity.

Example:
```
let favorite = bread_id.croissant
if ($favorite == bread_id.croissant) {
    echo "croissant selected"
}
```

### 4.4 Dynamic typing

Variables are dynamically typed:
```
let x = 5
let x = "hello"
```
## 5. Expressions
### 5.1 Arithmetic
`( expression )`

Examples:
```
let x = (3 + 4)
let y = ($x * 2)
```
Operators:

`+` `-` `*` `/`
`==` `!=` `>` `<` `>=` `<=`

### 5.2 Variable reference
`$x`

### 5.3 String interpolation
`"hello $name"`

### 5.4 Block expression
```
{
    statements
}
```
- Executes commands
- Captures stdout
- Returns string

## 6. Statements
### 6.1 Assignment
`let x = expression`

### 6.2 Command execution
`echo "hello"`
Executes system command or shell builtin

### 6.3 Function call
`myfunc(arg1, arg2)`

### 6.4 C function call
`c::puts("hello")`

### 6.5 Return
`return expression`

## 7 Control Flow
### 7.1 If
```
if (condition) {
    ...
} elif (condition) {
    ...
} else {
    ...
}
```
### 7.2 Loop
 - Infinite
```
loop {
}
```
- Counted
```
loop count N {
}
```
- Interval
```
loop interval N {
}
```
(N in seconds)

### 7.3 While
```
while (condition) {
}
```
### 7.4 Foreach
```
foreach item in array {
}
```
### 7.5 C-style for
```
for (init, condition, update) {
}
```

Example:
```
for (let i = 0, $i < 10, let i = ($i + 1)) {
}
```
### 7.6 Break
`break`

### 7.7 Scan (v1.1)
Scan provides enum-based branching in v1.1.

Syntax:
```
scan <expression> of <EnumType> {
    member: statement
    ...
}
```
A shorthand form omits the explicit expression and uses the nearest in-scope enum value of the given type:
```
scan of bread_id {
    brioche: french_function()
    baguette: french_function()
    fougasse: other_function()
    croissant: snack_function()
}
```
- Each branch label must be a member of the enum type.
- The matching branch executes when the scan value equals that member.
- If no branch matches, the scan returns nothing.

### 7.8 Switch (v1.1)
Switch provides C-style branching based on a selectable expression.

Syntax:
```
switch on <expression> {
    label: statement
    ...
    default: statement
}
```
- Branch labels are matched against the evaluated expression.
- If a matching branch is found, that branch executes.
- If no branch matches and a `default` branch is present, the default branch executes.
- If no match and no default is present, the switch returns nothing.

## 8.  Functions
### 8.1 Declaration
```
function name(param1, param2) {
    ...
}
```
### 8.2 Return
`return value`

### 8.3 Arguments

Passed by value (conceptually)

## 9. Error Handling
### 9.1 Try/Catch
```
try {
    ...
} catch {
    ...
}
```
catches command failures / runtime errors

## 10. Command Semantics
### 10.1 Command vs Function
```
Syntax	Meaning
echo "hi"	shell command
echo("hi")	ALSH function
c::puts("hi")	C function
```
### 10.2 Command execution
Uses system shell or direct exec
Return code determines success/failure

### 10.3 Block capture
`let x = { command }`
captures stdout
returns string

## 11. Execution Model Summary
- Preprocess (@define, @include, @import)
- Parse into AST
- solve @main / @justrunit
- Execute

## 12. Dataflow Models (Pipes, Pipelines, Chains)

ALSH supports three distinct dataflow systems. Each has a different semantic model and should not be confused.

### 12.1 Classic Shell Pipes (|)
Syntax:
`command1 | command2 | command3`

Semantics:
- Executes external system commands
- Uses raw stdout → stdin byte stream passing
- No type awareness
- No evaluation of ALSH functions unless explicitly invoked

Rules:
- Each segment is executed as a separate process
- Output of left command is streamed into stdin of right command
- Return code of the pipeline is the return code of the last command
- Fully compatible with POSIX-style shell behavior

Example:
`ls | grep ".c" | wc -l`

### 12.2 ALSH Pipelines (->)
Syntax:
command1 -> command2 -> command3

Semantics:
- Executes a stream-based transformation pipeline. 
- Each stage receives the stdout of the previous stage. 
- Still operates on string/byte streams, not structured values. 
- May include both system commands and ALSH functions.

Rules:
- Left-to-right evaluation
- Each stage is invoked sequentially
- Output of each stage becomes input to the next stage
- Functions and commands are treated uniformly if they accept string input

Example:
`ls -la -> grep txt -> tr a b`

or

`ls -> std::to_upper() -> print`

Key Property:

->` is syntactic sugar over stream passing, not structured evaluation.

### 12.3 Chains (chain {})
Syntax:
```
chain {
    expression1
    expression2
    expression3
}
```
Semantics:
Executes a value-based transformation pipeline
Each step operates on returned values, not raw streams
Output of each step is passed as an implicit argument (@) to the next step
Designed for structured, composable transformations

### 12.3.1 Input Binding (@ Placeholder)

Inside a chain block:

`@` represents the output of the previous step. 
If a function has exactly one parameter, `@` is implicitly applied. 
If a function has multiple parameters, `@` must be explicitly provided. 

### 12.3.2 Rules
#### Rule 1: Implicit single-argument binding
```
chain {
    "hello world"
    std::to_upper()
}
```
Expands to:
`std::to_upper(@)`

#### Rule 2: Explicit multi-argument binding
```
chain {
    "hello world"
    c::strstr(@, "world")
}
```
#### Rule 3: Zero-argument functions
Functions with no parameters are executed as-is:
```
chain {
    time()
    std::println()
}
```
#### Rule 4: Mixed expressions allowed
```
chain {
    readfile("data.txt")
    std::split("\n")
    std::len()
}
```
### 12.3.3 Return Behavior
```
The final expression in the chain is the return value
Chains always produce a value, not a stream
Typically returns:
string
number
array
struct
```
### 12.3.4 Example
```
chain {
    readfile("log.txt")
    std::split("\n")
    std::filter(@, contains("ERROR"))
    std::len()
}
```
### 12.4 Comparison of Dataflow Models
```
Model	Operator	Data Type	Execution	Purpose
Shell Pipe	`	`	bytes	external processes
ALSH Pipeline	->	streams	mixed commands/functions	scripting convenience
Chain	chain {}	values	AST evaluation	structured transformations
```
### 12.5 Design Principle
ALSH explicitly separates execution models:

Pipes = system-level IO
Pipelines = stream processing
Chains = structured computation

This avoids ambiguity between:

- processes
- text streams
- typed values

### 12.6 Interoperability Rules
### 12.6.1 Pipes and ALSH functions

Functions inside pipelines are implicitly wrapped:

`ls -> std::trim()`

treated as:

stdout → string → function input

### 12.6 Precedence

From lowest to highest abstraction:

- `|` (system shell)
- `->` (stream pipeline)
- `chain {}` (value pipeline)


# Some builtins/always available:
Aside from the shell builtins of alsh itself:
- `_bail` - instantly die with exit code 1
- `_exit(R)` - exit with a return code between 1 and 128

- Standard library functions are always available under `std::`, for example `std::print(...)`, `std::trim(...)`, `std::readfile(...)`, and `std::exists(...)`.
- Add `@stdlib` to a script to use these functions without the `std::` prefix.
