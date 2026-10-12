// The slice of the nrsml / nrs-lib-ts runtime that `<script>`s can see,
// evaluated once per `ScriptEngine` (see script.rs).
//
// Everything above the bridge is a type-stripped port of the real
// sources, kept close enough to diff against them:
//   - matrices/vectors and `ContextAPI`: nrs-lib-ts `src/math.ts`,
//     `src/process.ts`
//   - scopes, `executeScript`, `<contributor>` handling and
//     `acceptImpact`: nrsml `nrsml/parse.ts`
// The bridge at the bottom is the only part that doesn't exist upstream:
// it lets the Rust parser drive scope creation and read results back.

class Vector {
  constructor(data) {
    this.data = [...data];
  }

  copy() {
    return new Vector(this.data);
  }

  add(other) {
    if (this.data.length !== other.data.length) {
      throw new Error("dimension mismatch");
    }
    for (let i = 0; i < this.data.length; ++i) {
      this.data[i] += other.data[i];
    }
  }

  mul(scalar) {
    return new Vector(this.data.map((x) => x * scalar));
  }
}

class ScalarMatrix {
  constructor(data) {
    this.data = data;
  }

  copy() {
    return new ScalarMatrix(this.data);
  }

  get(i, j) {
    return i === j ? this.data : 0.0;
  }

  add(other) {
    if (other instanceof ScalarMatrix) {
      return new ScalarMatrix(this.data + other.data);
    }
    return other.add(this);
  }

  mul(rhs) {
    return rhs instanceof Vector ? this.matvecmul(rhs) : this.matmul(rhs);
  }

  scale(factor) {
    return new ScalarMatrix(this.data * factor);
  }

  matvecmul(vector) {
    return new Vector(vector.data.map((x) => x * this.data));
  }

  matmul(other) {
    if (other instanceof ScalarMatrix) {
      return new ScalarMatrix(this.data * other.data);
    }
    if (other instanceof DiagonalMatrix) {
      return new DiagonalMatrix(other.data.map((x) => x * this.data));
    }
    return new RegularMatrix(other.data.map((x) => x * this.data));
  }

  clamp01() {
    this.data = Math.min(1.0, this.data);
  }
}

class DiagonalMatrix {
  constructor(data) {
    this.data = [...data];
  }

  copy() {
    return new DiagonalMatrix(this.data);
  }

  get(i, j) {
    return i === j ? this.data[i] : 0.0;
  }

  add(matrix) {
    if (matrix instanceof ScalarMatrix) {
      return new DiagonalMatrix(this.data.map((x) => x + matrix.data));
    }
    if (matrix instanceof DiagonalMatrix) {
      return new DiagonalMatrix(this.data.map((x, i) => (x += matrix.data[i])));
    }
    return matrix.add(this);
  }

  mul(rhs) {
    return rhs instanceof Vector ? this.matvecmul(rhs) : this.matmul(rhs);
  }

  scale(factor) {
    return new DiagonalMatrix(this.data.map((x) => x * factor));
  }

  matvecmul(vector) {
    return new Vector(vector.data.map((x, i) => x * this.data[i]));
  }

  matmul(matrix) {
    if (matrix instanceof ScalarMatrix) {
      return matrix.mul(this);
    }
    if (matrix instanceof DiagonalMatrix) {
      return new DiagonalMatrix(this.data.map((x, i) => x * matrix.data[i]));
    }
    const n = this.data.length;
    return new RegularMatrix(matrix.data.map((x, i) => x * this.data[i % n]));
  }

  clamp01() {
    for (let i = 0; i < this.data.length; ++i) {
      this.data[i] = Math.min(1.0, this.data[i]);
    }
  }
}

class RegularMatrix {
  constructor(data) {
    this.data = [...data];
  }

  copy() {
    return new RegularMatrix(this.data);
  }

  get(i, j) {
    return this.data[i * this.dimensions() + j];
  }

  dimensions() {
    const n = Math.floor(Math.sqrt(this.data.length));
    if (n * n !== this.data.length) {
      throw new Error("invalid matrix size");
    }
    return n;
  }

  add(matrix) {
    if (matrix instanceof ScalarMatrix) {
      const n = this.dimensions();
      return new RegularMatrix(
        this.data.map((x, i) => x + (i % (n + 1) === 0 ? matrix.data : 0)),
      );
    }
    if (matrix instanceof DiagonalMatrix) {
      const n = this.dimensions();
      return new RegularMatrix(
        this.data.map(
          (x, i) => x + (i % (n + 1) === 0 ? matrix.data[i / (n + 1)] : 0),
        ),
      );
    }
    return new RegularMatrix(this.data.map((x, i) => x + matrix.data[i]));
  }

  clamp01() {
    for (let i = 0; i < this.data.length; ++i) {
      this.data[i] = Math.min(1.0, this.data[i]);
    }
  }

  mul(rhs) {
    return rhs instanceof Vector ? this.matvecmul(rhs) : this.matmul(rhs);
  }

  scale(factor) {
    return new RegularMatrix(this.data.map((x) => x * factor));
  }

  matvecmul(vector) {
    const n = this.dimensions();
    return new Vector(
      vector.data.map((x, i) => {
        let sum = 0.0;
        for (let j = 0; j < n; ++j) {
          sum += this.data[i * n + j] * x;
        }
        return sum;
      }),
    );
  }

  matmul(matrix) {
    if (matrix instanceof ScalarMatrix) {
      return matrix.mul(this);
    }
    const n = this.dimensions();
    if (matrix instanceof DiagonalMatrix) {
      return new RegularMatrix(
        this.data.map((x, i) => x * matrix.data[Math.floor(i / n)]),
      );
    }
    const result = new RegularMatrix(new Array(n * n).fill(0));
    for (let i = 0; i < n; ++i) {
      for (let j = 0; j < n; ++j) {
        for (let k = 0; k < n; ++k) {
          result.data[i * n + j] += this.data[k * n + j] * matrix.data[i * n + k];
        }
      }
    }
    return result;
  }
}

const identityMatrix = new ScalarMatrix(1.0);

class ContextAPI {
  newVector(data) {
    return new Vector(data);
  }
  newScalarMatrix(data) {
    return new ScalarMatrix(data);
  }
  newDiagonalMatrix(data) {
    return new DiagonalMatrix(data);
  }
  newRegularMatrix(data) {
    return new RegularMatrix(data);
  }
}

// nrsml's `containFactorEvaluator` default: a contributor factor is a JS
// expression ("0.5", "1/3"), not just a number literal.
function containFactorEvaluator(expression) {
  const value = new Function("return " + expression).apply(undefined);
  if (typeof value === "number") {
    return value;
  }
  throw new Error(expression);
}

function executeScript(scope, script, entryPoint) {
  new Function("global", "local", "scope", script).apply(scope, [
    scope.root.global,
    scope.local,
    scope,
  ]);
  if (entryPoint !== undefined) {
    scope.local[entryPoint]();
  }
}

// ---------------------------------------------------------------------
// Bridge: the Rust parser walks the XML and calls these in document
// order; scopes are referred to by integer handle.
// ---------------------------------------------------------------------

var __nrs = (() => {
  // processRoot's document scope. Includes are flattened before parsing
  // on the Rust side, so every file shares this one document scope
  // (upstream gives each included file its own, sharing only `global`).
  const root = {
    type: "document",
    value: { data: { entries: new Map(), impacts: [], relations: [] } },
    context: { api: new ContextAPI() },
    config: { containFactorEvaluator },
    macros: new Map(),
    global: {},
    local: {},
  };
  root.root = root;
  root.document = root;

  const scopes = [root];
  const open = (scope) => {
    scopes.push(scope);
    return scopes.length - 1;
  };
  const child = (parent, type, value) => ({
    type,
    parent,
    document: parent.document,
    entry: parent.entry,
    root: parent.root,
    value,
    local: Object.assign({}, parent.local),
    macros: new Map(parent.macros),
  });

  // Contributor matrices go back to Rust as plain JSON.
  const matrixJson = (m) => {
    if (m instanceof ScalarMatrix) return { kind: "scalar", value: m.data };
    if (m instanceof DiagonalMatrix) return { kind: "diagonal", value: m.data };
    if (m instanceof RegularMatrix) return { kind: "regular", value: m.data };
    return { kind: "unknown" };
  };
  const contributorsJson = (items) =>
    JSON.stringify(
      items.map((item) =>
        Array.from(item.contributors.entries()).map(([id, m]) => [id, matrixJson(m)]),
      ),
    );

  return {
    // processEntry
    openEntry(parentHandle, id, title, entryType) {
      const parent = scopes[parentHandle];
      const entry = {
        id,
        DAH_meta: { DAH_entry_title: title, DAH_entry_type: entryType },
      };
      parent.root.value.data.entries.set(id, entry);
      const scope = child(parent, "entry", entry);
      scope.entry = scope;
      return open(scope);
    },

    // processImpactBase; `count` > 1 for the paired shorthands
    // (maxAEIPADS, cryPADS). Upstream builds those as one scope over
    // several impacts that share a single contributors Map
    // (`DAH_standards.maxAEIPADS` passes the same Map to both halves),
    // so a contributor set on `value[0]` reaches all of them.
    openImpact(parentHandle, count) {
      const contributors = new Map();
      const value = [];
      for (let i = 0; i < count; ++i) {
        value.push({ contributors, DAH_meta: {} });
      }
      return open(child(scopes[parentHandle], "impact", value));
    },

    // processRelationBase
    openRelation(parentHandle) {
      const value = [{ contributors: new Map(), references: new Map(), DAH_meta: {} }];
      return open(child(scopes[parentHandle], "relation", value));
    },

    runScript(handle, script, entryPoint) {
      executeScript(scopes[handle], script, entryPoint ?? undefined);
    },

    // An impact's `<contributor>`, set on `impacts[0]`. `add`
    // returns a new matrix rather than mutating, so (as upstream) a
    // repeated id ends up as its last factor, clamped.
    impactContributor(handle, id, factorExpression) {
      const scope = scopes[handle];
      const impacts = scope.value;
      const factor = new ScalarMatrix(
        scope.root.config.containFactorEvaluator(factorExpression ?? "1.0"),
      );
      if (impacts[0].contributors.has(id)) {
        factor.add(impacts[0].contributors.get(id));
      }
      factor.clamp01();
      impacts[0].contributors.set(id, factor);
    },

    // A relation's `<contributor>`: applies to every relation in scope.
    relationContributor(handle, id, factor) {
      for (const relation of scopes[handle].value) {
        const factorMatrix = new ScalarMatrix(parseFloat(factor));
        if (relation.contributors.has(id)) {
          factorMatrix.add(relation.contributors.get(id));
        }
        factorMatrix.clamp01();
        relation.contributors.set(id, factorMatrix);
      }
    },

    // acceptImpact / the tail of processRelationBase: an item with no
    // contributors falls back to the enclosing entry, if there is one.
    // Returns each item's contributors and frees the handle.
    close(handle) {
      const scope = scopes[handle];
      scopes[handle] = undefined;
      if (scope.type === "entry") {
        return "null";
      }
      const sink =
        scope.type === "impact"
          ? scope.root.value.data.impacts
          : scope.root.value.data.relations;
      for (const item of scope.value) {
        sink.push(item);
        if (item.contributors.size == 0 && scope.entry !== undefined) {
          item.contributors.set(scope.entry.value.id, identityMatrix);
        }
      }
      return contributorsJson(scope.value);
    },
  };
})();
