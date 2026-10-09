# Language guide

TypedShell accepts a statically checked subset of TypeScript syntax from both
`.tsh` and `.ts` files. The parser is TypeScript-capable, but the compiler only
accepts constructs that have explicit TypedShell semantics. Generated scripts
are standalone and do not include a JavaScript runtime.

## Values and variables

The built-in scalar types are `string`, `number`, and `boolean`. `void` is used
for functions that return no value. Class names can also be used as types.

```ts
const greeting: string = "Hello";
let count: number = 1;
let ready = false; // the initializer gives this variable type boolean

count += 2;
ready = count > 0;
echo(`${greeting}, count=${count}, ready=${ready}`);
```

Use `const` for bindings that cannot be reassigned and `let` for mutable
bindings. Declarations need an initializer, and destructuring is not supported.
Local variables can infer a type from a supported initializer; annotations make
the intended type explicit.

`number` represents signed 64-bit integers. There are no floating-point values
or JavaScript coercions. Integer division truncates toward zero; Bash arithmetic
overflow follows Bash's integer behavior. Shell strings cannot contain NUL
bytes.

## Expressions

String literals, integer literals, booleans, and template literals are
supported. Template substitutions accept scalar values:

```ts
const project = "TypedShell";
const files = 3;
echo(`Building ${project}: ${files} files`);
```

Supported arithmetic operators are `+`, `-`, `*`, `/`, and `%`. `+` adds two
numbers or concatenates two strings. Equality checks compare values of the same
scalar type; relational comparisons support two numbers or two strings. Boolean
conditions use `!`, `&&`, and `||`; `&&` and `||` short-circuit. Conditions must
be booleans, so values are never implicitly treated as true or false.

Mutable numeric variables and fields support `++` and `--`. Assignment and
numeric compound assignments such as `+=` are supported for mutable variables
and instance fields.

## Control flow

The supported control statements are `if`/`else`, `while`, and C-style `for`,
including `break` and `continue` inside loops:

```ts
let index = 0;
while (index < 2) {
  echo(index);
  index++;
}

for (let item = 0; item < 3; item++) {
  if (item === 1) {
    continue;
  }
  echo(item);
}
```

Conditions must have type `boolean`. A `for` initializer and update expression
must be a variable declaration, assignment, or increment/decrement supported by
the compiler. Labeled loops and labeled `break`/`continue` are not supported.

## Functions

Function parameters need simple names and type annotations. Return types can be
written explicitly; the compiler can infer one when all return statements
resolve to a single type.

```ts
function factorial(value: number): number {
  if (value <= 1) {
    return 1;
  }
  return value * factorial(value - 1);
}

echo(factorial(5));
```

Functions can call and recursively call other functions. They can also read and
update top-level `let` bindings. Optional, default, rest, and destructured
parameters are not supported. Functions need a body; arrow functions and
function expressions are outside the subset.

## Classes

Classes support initialized instance fields, constructors, constructor parameter
properties, instance methods, and static methods. A field without an initializer
needs a type annotation and must be initialized by a constructor parameter
property or a direct assignment in the constructor. The compiler does not infer
initialization through conditional branches.

```ts
class Counter {
  value: number = 0;

  constructor(public name: string) {}

  next(): number {
    this.value++;
    return this.value;
  }

  static kind(): string {
    return "counter";
  }
}

const counter = new Counter("jobs");
echo(`${Counter.kind()}: ${counter.name} ${counter.next()}`);
```

Class references are valid parameter, return, field, and local variable types.
Instance fields are mutable. Inheritance, interfaces and `implements`, static
fields, accessors, decorators, private/protected members, readonly fields, and
optional or computed members are not supported.

## Modules

Relative `.tsh` and `.ts` modules can export and import named functions, classes,
and values:

```ts
// greeting.tsh
export function greeting(name: string): string {
  return `Hello, ${name}!`;
}
```

```ts
// main.tsh
import { greeting } from "./greeting.tsh";
echo(greeting("Bash"));
```

The compiler resolves each import relative to the file that contains it, checks
the imported name, and bundles local modules into the output. Cycles, package
imports, default imports, namespace imports, and import aliases are not
supported. Built-in `tsh:` modules are described in
[compile-time selection](compile-time.md).

## Outside the supported subset

General arrays and objects, `var`, destructuring, unions and generics, arrow
functions, optional chaining, nullish coalescing, ternaries, async/await,
promises, generators, inheritance, decorators, dynamic imports, and arbitrary
JavaScript or Node APIs are not implemented. Arrays and literal option objects
are accepted only in specific shell APIs such as `run()` and `mkdir()`; see
[Shell APIs](shell-apis.md).
