# (kinda) alsh standard library
these are always available
if you don't want to write std::, then use @stdlib in the file

## Primitives
these need to be fast; and they're baked into the shell (in Rust)

std::print(...)         // like echo but a proper function
std::println(...)       // same but add a newline
std::eprint(...)        // stderr
std::input(prompt)      // user input, replacess read/fgets
std::exit(code)         // clean exit


## c:: safe wrappers
these wrap C functions

std::env(key)           // c::getenv but returns "" instead of null
std::env(key, default)  // with fallback, the actually useful version
std::readfile(path)     // wraps fopen/fread/fclose, returns string
std::writefile(path, str) // the other way around
std::appendfile(path, str)
std::exists(path)       // stat() wrapped, returns bool


## string utilities
some of these wrap C functions, other are written in alsh, others in Rust

std::strlen(s)          // wraps strlen() but safe
std::upper(s)           // wraps toupper per-char
std::lower(s)
std::trim(s)            // implemented in Rust
std::contains(s, sub)   // wraps strstr, returns bool instead of pointer
std::replace(s, source, dest) 
std::split(s, sep)      // returns array
std::lines(s)          // returns an array of lines from the string
std::startswith(s, sub) // like contains, but `sub` is at the start of `s`
std::endswith(s, sub)   // like contains, but `sub` is at the end of `s`
std::padleft(s, width, char)
std::padright(s, width, char)
std::repeat(s, n)        # "─" repeated 40 times, you used this in sysinfo
std::lines(s)            # split on newlines, returns array. youll use this constantly
std::strip(s, chars)     # trim but specify what to strip, not just whitespace

## numeric helpers
std::format_gb(kb)      // convert kilobytes to decimal gigabytes
std::format_gib(kb)     // convert kilobytes to gibibytes

## others
std::sizeof(value)     // returns the in-memory size of a value

## array stuff
std::len(arr)            # you have strlen, need this for arrays too
std::push(arr, val)      # append
std::pop(arr)            # remove last
std::first(arr)          # arr[0] but safe
std::last(arr)           # arr[n-1] but safe
std::slice(arr, from, to)
std::contains(arr, val)  # you have this for strings, need it for arrays too
std::reverse(arr)
std::join(arr, sep)      # inverse of split, you'll need this constantly

## math stuff that's annoying to do with c::
std::min(a, b)
std::max(a, b)
std::clamp(val, min, max)
std::even(n)
std::odd(n)

## path and fs stuff
std::basename(path)      # "/home/user/file.txt" -> "file.txt"
std::dirname(path)       # "/home/user/file.txt" -> "/home/user"
std::joinpath(a, b)      # handles slashes correctly
std::isfile(path)
std::isdir(path)
std::mkdir(path)
std::listdir(path)       # returns array of filenames

## process/system stuff
std::which(cmd)          # which binary would run for this command
std::pid()               # current process id
std::sleep(ms)           # milliseconds, more useful than c::sleep seconds
std::getuser()           # env("USER") but explicit