# aider chat started at 2026-04-01 10:00:00

#### add a fibonacci function to math.py

> I'll add `fib(n)` to math.py:
>
> ```python
> def fib(n):
>     a, b = 0, 1
>     for _ in range(n):
>         a, b = b, a + b
>     return a
> ```
>
> Saved math.py.

#### now write a quick test for it

> Added test_math.py with one assertion:
>
> ```python
> from math import fib
> assert fib(10) == 55
> ```
>
> Tests pass.
