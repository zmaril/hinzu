// A tiny fixture for the TypeScript structural extractor: two async loaders that
// are structurally near-identical and whose RESOLVED return types (Promise<User>
// and Promise<Order>) erase to the same shape Promise<_>, so the checker-backed
// extractor matches them. `sumTotals` is unrelated and must not join.
export interface User {
  id: string;
  name: string;
}
export interface Order {
  id: number;
  total: number;
}

async function fetchRow(id: string): Promise<Record<string, unknown> | null> {
  return id.length > 0 ? {} : null;
}

function parse(row: Record<string, unknown>): unknown {
  return row;
}

export async function loadUser(id: string): Promise<User> {
  const row = await fetchRow(id);
  if (!row) {
    throw new Error("not found");
  }
  return parse(row) as User;
}

export async function loadOrder(id: string): Promise<Order> {
  const row = await fetchRow(id);
  if (!row) {
    throw new Error("not found");
  }
  return parse(row) as Order;
}

export function sumTotals(orders: Order[]): number {
  let total = 0;
  for (const o of orders) {
    total += o.total;
  }
  return total;
}
