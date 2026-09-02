---
name: Format not parsed
about: An export fails to load, or the numbers look implausible
labels: format
---

Apple publishes no schema, and the format changes between iOS releases.
Reports like this are the main way we find out.

**Export version.** The first lines of `export.xml`:

```
<!-- HealthKit Export Version: NN -->
```

**iOS / watchOS version:**

**What went wrong:**

**Markup excerpt.** A few elements that show the problem.
⚠️ Strip the values and dates — the structure is what matters, not your
measurements. Never attach the whole `export.xml`: it is medical data.

```xml

```

**Command output:**

```
```
