import { Transaction as $ } from "@mysten/sui/transactions";
import { bcs as s, toHex as H, fromHex as q } from "@mysten/bcs";
import { SuiClient as z, getFullnodeUrl as O } from "@mysten/sui/client";
import W from "lodash.camelcase";
import { normalizeStructTag as K } from "@mysten/sui/utils";
import { SuiPriceServiceConnection as E, SuiPythClient as Y } from "@pythnetwork/pyth-sui-js";
import T from "bignumber.js";
import { bcs as b } from "@mysten/sui/bcs";
const R = s.bytes(32).transform({
  // To change the input type, you need to provide a type definition for the input
  input: (e) => q(e),
  output: (e) => H(e)
}), pe = s.struct("IncentiveAPYInfo", {
  /** Asset identifier */
  asset_id: s.u8(),
  /** Annual Percentage Yield as a 256-bit integer */
  apy: s.u256(),
  /** List of supported coin types for this incentive */
  coin_types: s.vector(s.string())
}), J = s.struct("IncentivePoolInfo", {
  /** Unique pool identifier */
  pool_id: R,
  /** Address holding the incentive funds */
  funds: R,
  /** Current phase of the incentive program */
  phase: s.u64(),
  /** Timestamp when the incentive started */
  start_at: s.u64(),
  /** Timestamp when the incentive ends */
  end_at: s.u64(),
  /** Timestamp when the incentive was closed */
  closed_at: s.u64(),
  /** Total supply of incentive tokens */
  total_supply: s.u64(),
  /** Asset identifier for the incentive */
  asset_id: s.u8(),
  /** Option type for the incentive */
  option: s.u8(),
  /** Factor used in incentive calculations */
  factor: s.u256(),
  /** Amount of incentives already distributed */
  distributed: s.u64(),
  /** Amount of incentives currently available */
  available: s.u256(),
  /** Total amount of incentives */
  total: s.u256()
}), fe = s.struct("IncentivePoolInfoByPhase", {
  /** Phase number */
  phase: s.u64(),
  /** List of incentive pools in this phase */
  pools: s.vector(J)
}), me = s.struct("OracleInfo", {
  /** Oracle identifier */
  oracle_id: s.u8(),
  /** Current price as a 256-bit integer */
  price: s.u256(),
  /** Number of decimal places for the price */
  decimals: s.u8(),
  /** Whether the oracle data is valid */
  valid: s.bool()
}), ge = s.struct("FlashLoanAssetConfig", {
  /** Unique identifier for the flash loan asset */
  id: s.string(),
  /** Asset identifier */
  asset_id: s.u8(),
  /** Coin type for the asset */
  coin_type: s.string(),
  /** Pool identifier for the flash loan */
  pool_id: s.string(),
  /** Rate paid to suppliers for flash loans */
  rate_to_supplier: s.u64(),
  /** Rate paid to treasury for flash loans */
  rate_to_treasury: s.u64(),
  /** Maximum flash loan amount */
  max: s.u64(),
  /** Minimum flash loan amount */
  min: s.u64()
}), ye = s.struct("ReserveDataInfo", {
  /** Reserve identifier */
  id: s.u8(),
  /** Oracle identifier for price feeds */
  oracle_id: s.u8(),
  /** Coin type for the reserve */
  coin_type: s.string(),
  /** Maximum supply capacity */
  supply_cap: s.u256(),
  /** Maximum borrow capacity */
  borrow_cap: s.u256(),
  /** Current supply interest rate */
  supply_rate: s.u256(),
  /** Current borrow interest rate */
  borrow_rate: s.u256(),
  /** Current supply index for interest calculation */
  supply_index: s.u256(),
  /** Current borrow index for interest calculation */
  borrow_index: s.u256(),
  /** Total amount supplied to the reserve */
  total_supply: s.u256(),
  /** Total amount borrowed from the reserve */
  total_borrow: s.u256(),
  /** Timestamp of last update */
  last_update_at: s.u64(),
  /** Loan-to-Value ratio for collateral */
  ltv: s.u256(),
  /** Treasury factor for fee calculations */
  treasury_factor: s.u256(),
  /** Current treasury balance */
  treasury_balance: s.u256(),
  /** Base interest rate */
  base_rate: s.u256(),
  /** Interest rate multiplier */
  multiplier: s.u256(),
  /** Jump rate multiplier for high utilization */
  jump_rate_multiplier: s.u256(),
  /** Reserve factor for protocol fees */
  reserve_factor: s.u256(),
  /** Optimal utilization rate */
  optimal_utilization: s.u256(),
  /** Liquidation ratio threshold */
  liquidation_ratio: s.u256(),
  /** Liquidation bonus for liquidators */
  liquidation_bonus: s.u256(),
  /** Liquidation threshold */
  liquidation_threshold: s.u256()
}), Q = s.struct("UserStateInfo", {
  /** Asset identifier */
  asset_id: s.u8(),
  /** User's current borrow balance */
  borrow_balance: s.u256(),
  /** User's current supply balance */
  supply_balance: s.u256()
}), I = new z({
  url: O("mainnet")
});
function N(e) {
  const a = [];
  return e.forEach((r, t) => {
    const c = t === e.length - 1;
    if (typeof r == "object" && c) {
      const { client: n, disableCache: o, cacheTime: i, ...u } = r;
      a.push(u);
    } else
      a.push(r);
  }), JSON.stringify(a);
}
function _(e) {
  const a = {};
  return (...r) => {
    const t = N(r);
    return a[t] || (a[t] = e(...r).finally(() => {
      a[t] = null;
    })), a[t];
  };
}
function A(e) {
  let a = {};
  return (...r) => {
    const t = r[r.length - 1], c = N(r), n = a[c];
    return !(t != null && t.disableCache) && typeof (n == null ? void 0 : n.data) != "undefined" && (typeof (t == null ? void 0 : t.cacheTime) == "undefined" || t.cacheTime > Date.now() - n.cacheAt) ? n.data : e(...r).then((o) => (a[c] = {
      data: o,
      cacheAt: Date.now()
    }, o));
  };
}
function B(e) {
  return Array.isArray(e) ? e.map((a) => B(a)) : e != null && typeof e == "object" ? Object.keys(e).reduce(
    (a, r) => ({
      ...a,
      [W(r)]: B(e[r])
    }),
    {}
  ) : e;
}
function f(e, a) {
  return typeof e == "object" ? e : a(e);
}
function X(e, a) {
  return typeof a == "string" ? e.object(a) : typeof a == "object" && a.$kind ? a : e.object(a.contract.pool);
}
function k(e, a, r) {
  if (e.results && e.results.length > 0) {
    if (e.results[0].returnValues && e.results[0].returnValues.length > 0)
      return e.results[0].returnValues.map((t, c) => (a[c] || a[0]).parse(Uint8Array.from(t[0])));
  } else if (e.error)
    return console.log(`Get an error, msg: ${e.error}`), [];
  return [];
}
function m(e) {
  return K(e);
}
function L(e) {
  const a = (e || 0) / Math.pow(10, 27);
  return a > Math.pow(10, 5) ? 1 / 0 : a;
}
new E("https://hermes.pyth.network", {
  timeout: 2e4
});
const Z = 27, V = (e, a) => {
  if (!Number(e) || !Number(a)) return new T(0);
  const r = new T(1).shiftedBy(1 * Z), t = r.multipliedBy(new T(0.5));
  return new T(e).multipliedBy(new T(a)).plus(t).dividedBy(r).integerValue(T.ROUND_DOWN);
}, w = A(
  _(async (e) => {
    const a = `https://open-api.naviprotocol.io/api/navi/config?env=${(e == null ? void 0 : e.env) || "prod"}`;
    return (await fetch(a).then((t) => t.json())).data;
  })
), g = 1e3 * 60 * 5;
var P = /* @__PURE__ */ ((e) => (e[e.Supply = 1] = "Supply", e[e.Withdraw = 2] = "Withdraw", e[e.Borrow = 3] = "Borrow", e[e.Repay = 4] = "Repay", e))(P || {});
const S = A(
  _(async (e) => {
    const a = `https://open-api.naviprotocol.io/api/navi/pools?env=${(e == null ? void 0 : e.env) || "prod"}`;
    return (await fetch(a).then((t) => t.json())).data;
  })
);
async function C(e, a) {
  const r = await S({
    ...a,
    cacheTime: g
  });
  if (typeof e == "object")
    return e;
  const t = r.find((c) => typeof e == "string" ? m(c.suiCoinType) === m(e) : typeof e == "number" ? c.id === e : !1);
  if (!t)
    throw new Error("Pool not found");
  return t;
}
const we = A(
  _(async (e) => (await fetch("https://open-api.naviprotocol.io/api/navi/stats").then((t) => t.json())).data)
), he = A(
  _(
    async (e) => await fetch("https://open-api.naviprotocol.io/api/navi/fee").then((t) => t.json())
  )
);
async function x(e, a, r, t) {
  const c = await w({
    ...t,
    cacheTime: g
  }), n = await C(a, t), o = typeof r == "object" && r.$kind === "GasCoin";
  if (m(n.suiCoinType) === m("0x2::sui::SUI") && o) {
    if (!(t != null && t.amount))
      throw new Error("Amount is required for sui coin");
    r = e.splitCoins(r, [t.amount]);
  }
  let i;
  return typeof (t == null ? void 0 : t.amount) != "undefined" ? i = f(t.amount, e.pure.u64) : i = e.moveCall({
    target: "0x2::coin::value",
    arguments: [f(r, e.object)],
    typeArguments: [n.suiCoinType]
  }), t != null && t.accountCap ? e.moveCall({
    target: `${c.package}::incentive_v3::deposit_with_account_cap`,
    arguments: [
      e.object("0x06"),
      e.object(c.storage),
      e.object(n.contract.pool),
      e.pure.u8(n.id),
      f(r, e.object),
      e.object(c.incentiveV2),
      e.object(c.incentiveV3),
      f(t.accountCap, e.object)
    ],
    typeArguments: [n.suiCoinType]
  }) : e.moveCall({
    target: `${c.package}::incentive_v3::entry_deposit`,
    arguments: [
      e.object("0x06"),
      e.object(c.storage),
      e.object(n.contract.pool),
      e.pure.u8(n.id),
      f(r, e.object),
      i,
      e.object(c.incentiveV2),
      e.object(c.incentiveV3)
    ],
    typeArguments: [n.suiCoinType]
  }), e;
}
async function be(e, a, r, t) {
  const c = await w({
    ...t,
    cacheTime: g
  }), n = await C(a, t), o = f(r, e.pure.u64);
  let i;
  if (t != null && t.accountCap) {
    const [l] = e.moveCall({
      target: `${c.package}::incentive_v3::withdraw_with_account_cap`,
      arguments: [
        e.object("0x06"),
        e.object(c.priceOracle),
        e.object(c.storage),
        e.object(n.contract.pool),
        e.pure.u8(n.id),
        o,
        e.object(c.incentiveV2),
        e.object(c.incentiveV3),
        f(t.accountCap, e.object)
      ],
      typeArguments: [n.suiCoinType]
    });
    i = l;
  } else {
    const [l] = e.moveCall({
      target: `${c.package}::incentive_v3::withdraw`,
      arguments: [
        e.object("0x06"),
        e.object(c.priceOracle),
        e.object(c.storage),
        e.object(n.contract.pool),
        e.pure.u8(n.id),
        o,
        e.object(c.incentiveV2),
        e.object(c.incentiveV3)
      ],
      typeArguments: [n.suiCoinType]
    });
    i = l;
  }
  return e.moveCall({
    target: "0x2::coin::from_balance",
    arguments: [i],
    typeArguments: [n.suiCoinType]
  });
}
async function ve(e, a, r, t) {
  const c = await w({
    ...t,
    cacheTime: g
  }), n = await C(a, t), o = f(r, e.pure.u64);
  let i;
  if (t != null && t.accountCap) {
    const [l] = e.moveCall({
      target: `${c.package}::incentive_v3::borrow_with_account_cap`,
      arguments: [
        e.object("0x06"),
        e.object(c.priceOracle),
        e.object(c.storage),
        e.object(n.contract.pool),
        e.pure.u8(n.id),
        o,
        e.object(c.incentiveV2),
        e.object(c.incentiveV3),
        f(t.accountCap, e.object)
      ],
      typeArguments: [n.suiCoinType]
    });
    i = l;
  } else {
    const [l] = e.moveCall({
      target: `${c.package}::incentive_v3::borrow`,
      arguments: [
        e.object("0x06"),
        e.object(c.priceOracle),
        e.object(c.storage),
        e.object(n.contract.pool),
        e.pure.u8(n.id),
        o,
        e.object(c.incentiveV2),
        e.object(c.incentiveV3)
      ],
      typeArguments: [n.suiCoinType]
    });
    i = l;
  }
  return e.moveCall({
    target: "0x2::coin::from_balance",
    arguments: [e.object(i)],
    typeArguments: [n.suiCoinType]
  });
}
async function Ce(e, a, r, t) {
  const c = await w({
    ...t,
    cacheTime: g
  }), n = await C(a, t), o = typeof r == "object" && r.$kind === "GasCoin";
  if (m(n.suiCoinType) === m("0x2::sui::SUI") && o) {
    if (!(t != null && t.amount))
      throw new Error("Amount is required for sui coin");
    r = e.splitCoins(r, [t.amount]);
  }
  let i;
  return typeof (t == null ? void 0 : t.amount) != "undefined" ? i = f(t.amount, e.pure.u64) : i = e.moveCall({
    target: "0x2::coin::value",
    arguments: [f(r, e.object)],
    typeArguments: [n.suiCoinType]
  }), t != null && t.accountCap ? e.moveCall({
    target: `${c.package}::incentive_v3::repay_with_account_cap`,
    arguments: [
      e.object("0x06"),
      e.object(c.priceOracle),
      e.object(c.storage),
      e.object(n.contract.pool),
      e.pure.u8(n.id),
      f(r, e.object),
      i,
      e.object(c.incentiveV2),
      e.object(c.incentiveV3),
      f(t.accountCap, e.object)
    ],
    typeArguments: [n.suiCoinType]
  }) : e.moveCall({
    target: `${c.package}::incentive_v3::entry_repay`,
    arguments: [
      e.object("0x06"),
      e.object(c.priceOracle),
      e.object(c.storage),
      e.object(n.contract.pool),
      e.pure.u8(n.id),
      f(r, e.object),
      i,
      e.object(c.incentiveV2),
      e.object(c.incentiveV3)
    ],
    typeArguments: [n.suiCoinType]
  }), e;
}
const _e = A(
  _(async (e) => {
    const a = await w({
      ...e
    }), t = (await I.getObject({
      id: a.incentiveV3,
      options: { showType: !0, showOwner: !0, showContent: !0 }
    })).data.content.fields.borrow_fee_rate;
    return Number(t) / 100;
  })
);
function je(e, a, r) {
  const t = typeof (r == null ? void 0 : r.balance) == "number", c = t ? r.balance : 0;
  let n = 0;
  const o = [];
  let i = "";
  if (a.sort((l, d) => Number(d.balance) - Number(l.balance)).forEach((l) => {
    if (!(t && n >= c) && Number(l.balance) !== 0) {
      if (i || (i = l.coinType), i !== l.coinType)
        throw new Error("All coins must be of the same type");
      n += Number(l.balance), o.push(l.coinObjectId);
    }
  }), o.length === 0)
    throw new Error("No coins to merge");
  if (t && n < c)
    throw new Error(
      `Balance is less than the specified balance: ${n} < ${c}`
    );
  if (m(i) === m("0x2::sui::SUI") && (r != null && r.useGasCoin))
    return t ? e.splitCoins(e.gas, [e.pure.u64(c)]) : e.gas;
  const u = o.length === 1 ? e.object(o[0]) : e.mergeCoins(o[0], o.slice(1));
  return t ? e.splitCoins(u, [e.pure.u64(c)]) : u;
}
async function D(e, a, r, t, c, n, o) {
  const i = await w({
    ...o,
    cacheTime: g
  }), u = await C(r, o);
  return e.moveCall({
    target: `${i.uiGetter}::calculator_unchecked::dynamic_health_factor`,
    arguments: [
      e.object("0x06"),
      e.object(i.storage),
      e.object(i.oracle.priceOracle),
      X(e, u),
      f(a, e.pure.address),
      f(u.id, e.pure.u8),
      f(t, e.pure.u64),
      f(c, e.pure.u64),
      f(n, e.pure.bool)
    ],
    typeArguments: [u.suiCoinType]
  });
}
async function ee(e, a, r) {
  return D(e, a, 0, 0, 0, !1, r);
}
const Te = A(
  async (e, a) => {
    var l;
    const r = await w({
      ...a,
      cacheTime: g
    }), t = new $(), c = (l = a == null ? void 0 : a.client) != null ? l : I, n = await S(a);
    t.moveCall({
      target: `${r.uiGetter}::getter_unchecked::get_user_state`,
      arguments: [t.object(r.storage), t.pure.address(e)]
    });
    const o = await c.devInspectTransactionBlock({
      transactionBlock: t,
      sender: e
    }), i = k(o, [b.vector(Q)]);
    return B(
      i[0].filter((d) => d.supply_balance !== "0" || d.borrow_balance !== "0")
    ).map((d) => {
      const h = n.find((v) => v.id === d.assetId), p = V(
        d.supplyBalance,
        h.currentSupplyIndex
      ).toString(), y = V(
        d.borrowBalance,
        h.currentBorrowIndex
      ).toString();
      return {
        ...d,
        supplyBalance: p,
        borrowBalance: y,
        pool: h
      };
    }).filter((d) => !!d.pool);
  }
);
async function Ie(e, a) {
  var o;
  const r = (o = a == null ? void 0 : a.client) != null ? o : I, t = new $();
  await ee(t, e, a);
  const c = await r.devInspectTransactionBlock({
    transactionBlock: t,
    sender: e
  }), n = k(c, [b.u256()]);
  return L(Number(n[0]) || 0);
}
async function Ae(e, a, r, t) {
  var p;
  const c = (p = t == null ? void 0 : t.client) != null ? p : I, n = new $();
  let o = 0, i = 0;
  const u = await C(a, t);
  if (r.forEach((y) => {
    y.type === P.Supply ? o += y.amount : y.type === P.Withdraw ? o -= y.amount : y.type === P.Borrow ? i += y.amount : y.type === P.Repay && (i -= y.amount);
  }), o * i < 0)
    throw new Error("Invalid operations");
  const l = o > 0 || i > 0;
  await D(
    n,
    e,
    u,
    Math.abs(o),
    Math.abs(i),
    l,
    t
  );
  const d = await c.devInspectTransactionBlock({
    transactionBlock: n,
    sender: e
  }), h = k(d, [b.u256()]);
  return L(Number(h[0]) || 0);
}
const Pe = _(
  async (e, a) => {
    const r = new URLSearchParams();
    a != null && a.cursor && r.set("cursor", a.cursor), r.set("userAddress", e);
    const t = `https://open-api.naviprotocol.io/api/navi/user/transactions?${r.toString()}`;
    return (await fetch(t).then((n) => n.json())).data;
  }
);
async function Be(e, a) {
  var n;
  let r = null;
  const t = [], c = (n = a == null ? void 0 : a.client) != null ? n : I;
  do {
    let o;
    if (a != null && a.coinType ? o = await c.getCoins({
      owner: e,
      coinType: a == null ? void 0 : a.coinType,
      cursor: r,
      limit: 100
    }) : o = await c.getAllCoins({
      owner: e,
      cursor: r,
      limit: 100
    }), !o.data || !o.data.length)
      break;
    t.push(...o.data), r = o.nextCursor;
  } while (r);
  return t;
}
const M = new E("https://hermes.pyth.network", {
  timeout: 1e4
});
async function te(e) {
  try {
    const a = [], r = await M.getLatestPriceFeeds(e);
    if (!r) return a;
    const t = Math.floor((/* @__PURE__ */ new Date()).valueOf() / 1e3);
    for (const c of r) {
      const n = c.getPriceUnchecked();
      if (n.publishTime > t) {
        console.warn(
          `pyth price feed is invalid, id: ${c.id}, publish time: ${n.publishTime}, current timestamp: ${t}`
        );
        continue;
      }
      t - c.getPriceUnchecked().publishTime > 30 && (console.info(
        `stale price feed, id: ${c.id}, publish time: ${n.publishTime}, current timestamp: ${t}`
      ), a.push(c.id));
    }
    return a;
  } catch (a) {
    throw new Error(`failed to get pyth stale price feed id, msg: ${a.message}`);
  }
}
async function ae(e, a, r) {
  var n;
  const t = (n = r == null ? void 0 : r.client) != null ? n : I, c = await w({
    ...r,
    cacheTime: g
  });
  try {
    const o = await M.getPriceFeedsUpdateData(a);
    return await new Y(
      t,
      c.oracle.pythStateId,
      c.oracle.wormholeStateId
    ).updatePriceFeeds(e, o, a);
  } catch (o) {
    throw new Error(`failed to update pyth price feeds, msg: ${o.message}`);
  }
}
async function $e(e, a, r) {
  const t = await w({
    ...r,
    cacheTime: g
  });
  if (r != null && r.updatePythPriceFeeds) {
    const c = a.filter((n) => !!n.pythPriceFeedId).map((n) => n.pythPriceFeedId);
    try {
      const n = await te(c);
      n.length > 0 && await ae(e, n, r);
    } catch {
    }
  }
  for (const c of a)
    e.moveCall({
      target: `${t.oracle.packageId}::oracle_pro::update_single_price`,
      arguments: [
        e.object("0x6"),
        // Clock object
        e.object(t.oracle.oracleConfig),
        // Oracle configuration
        e.object(t.oracle.priceOracle),
        // Price oracle contract
        e.object(t.oracle.supraOracleHolder),
        // Supra oracle holder
        e.object(c.pythPriceInfoObject),
        // Pyth price info object
        e.pure.address(c.feedId)
        // Price feed ID
      ]
    });
  return e;
}
async function re(e) {
  return (await w({
    ...e,
    cacheTime: g
  })).oracle.feeds;
}
function ke(e, a) {
  return e.filter((r) => !!(a != null && a.lendingState && a.lendingState.find((c) => c.assetId === r.assetId) || a != null && a.pools && a.pools.find((c) => c.id === r.assetId)));
}
const F = A(
  _(async (e) => {
    const a = `https://open-api.naviprotocol.io/api/navi/flashloan?env=${(e == null ? void 0 : e.env) || "prod"}`, r = await fetch(a).then((t) => t.json());
    return Object.keys(r.data).map((t) => ({
      ...r.data[t],
      coinType: t
    }));
  })
);
async function Se(e, a) {
  return (await F(a)).find((t) => typeof e == "string" ? m(t.coinType) === m(e) : typeof e == "number" ? t.assetId === e : t.assetId === e.id) || null;
}
async function Fe(e, a, r, t) {
  const c = await w({
    ...t,
    cacheTime: g
  }), n = await C(a, t);
  if (!(await F({
    ...t,
    cacheTime: g
  })).some(
    (d) => m(d.coinType) === m(n.suiCoinType)
  ))
    throw new Error("Pool does not support flashloan");
  const [u, l] = e.moveCall({
    target: `${c.package}::lending::flash_loan_with_ctx`,
    arguments: [
      e.object(c.flashloanConfig),
      e.object(n.contract.pool),
      f(r, e.pure.u64)
    ],
    typeArguments: [n.suiCoinType]
  });
  return [u, l];
}
async function Re(e, a, r, t, c) {
  const n = await w({
    ...c,
    cacheTime: g
  }), o = await C(a, c);
  if (!(await F({
    ...c,
    cacheTime: g
  })).some(
    (d) => m(d.coinType) === m(o.suiCoinType)
  ))
    throw new Error("Pool does not support flashloan");
  const [l] = e.moveCall({
    target: `${n.package}::lending::flash_repay_with_ctx`,
    arguments: [
      e.object("0x06"),
      e.object(n.storage),
      e.object(o.contract.pool),
      f(r, e.object),
      f(t, e.object)
    ],
    typeArguments: [o.suiCoinType]
  });
  return [l];
}
async function Ve(e, a, r, t, c, n) {
  const o = {
    ...n,
    cacheTime: g
  }, i = await w(o), u = await C(a, o), l = await C(t, o), [d, h] = e.moveCall({
    target: `${i.package}::incentive_v3::liquidation`,
    arguments: [
      e.object("0x06"),
      // Clock object
      e.object(i.priceOracle),
      // Price oracle for asset pricing
      e.object(i.storage),
      // Protocol storage
      e.pure.u8(u.id),
      // Pay asset ID
      e.object(u.contract.pool),
      // Pay asset pool contract
      f(r, e.object),
      // Debt repayment amount
      e.pure.u8(l.id),
      // Collateral asset ID
      e.object(l.contract.pool),
      // Collateral asset pool contract
      f(c, e.pure.address),
      // Borrower address
      e.object(i.incentiveV2),
      // Incentive V2 contract
      e.object(i.incentiveV3)
      // Incentive V3 contract
    ],
    typeArguments: [u.suiCoinType, l.suiCoinType]
  });
  return [d, h];
}
async function Ee(e, a) {
  var d;
  const r = await re(a), t = await S({
    ...a,
    cacheTime: g
  }), c = (d = a == null ? void 0 : a.client) != null ? d : I, n = await w({
    ...a,
    cacheTime: g
  }), o = new $();
  o.moveCall({
    target: `${n.uiGetter}::incentive_v3_getter::get_user_atomic_claimable_rewards`,
    arguments: [
      o.object("0x06"),
      // Clock object
      o.object(n.storage),
      // Protocol storage
      o.object(n.incentiveV3),
      // Incentive V3 contract
      o.pure.address(e)
      // User address
    ]
  });
  const i = await c.devInspectTransactionBlock({
    transactionBlock: o,
    sender: e
  }), u = k(
    i,
    [
      b.vector(b.string()),
      // Asset coin types
      b.vector(b.string()),
      // Reward coin types
      b.vector(b.u8()),
      // Reward options
      b.vector(b.Address),
      // Rule IDs
      b.vector(b.u256())
      // Claimable amounts
    ]
  ), l = [];
  if (u.length === 5 && Array.isArray(u[0])) {
    const h = u[0].length;
    for (let p = 0; p < h; p++) {
      const y = r.find(
        (j) => m(j.coinType) === m(u[1][p])
      ), v = t.find(
        (j) => m(j.coinType) === m(u[0][p])
      );
      !y || !v || l.push({
        assetId: v.id,
        assetCoinType: m(u[0][p]),
        rewardCoinType: m(u[1][p]),
        option: Number(u[2][p]),
        userClaimableReward: Number(u[4][p]) / Math.pow(10, y.priceDecimal),
        ruleIds: Array.isArray(u[3][p]) ? u[3][p] : [u[3][p]]
      });
    }
  }
  return l;
}
function Ne(e) {
  const a = /* @__PURE__ */ new Map();
  e.forEach((t) => {
    const c = t.assetId, n = t.option, o = `${c}-${n}-${t.rewardCoinType}`;
    a.has(o) ? a.get(o).total += t.userClaimableReward : a.set(o, {
      assetId: c,
      rewardType: n,
      coinType: t.rewardCoinType,
      total: Number(t.userClaimableReward)
    });
  });
  const r = /* @__PURE__ */ new Map();
  for (const { assetId: t, rewardType: c, coinType: n, total: o } of a.values()) {
    const i = `${t}-${c}`;
    r.has(i) || r.set(i, { assetId: t, rewardType: c, rewards: /* @__PURE__ */ new Map() });
    const u = r.get(i);
    u.rewards.set(n, (u.rewards.get(n) || 0) + o);
  }
  return Array.from(r.values()).map((t) => ({
    assetId: t.assetId,
    rewardType: t.rewardType,
    rewards: Array.from(t.rewards.entries()).map(([c, n]) => ({
      coinType: c,
      available: n.toFixed(6)
    }))
  }));
}
const Le = _(
  async (e) => {
    const a = `https://open-api.naviprotocol.io/api/navi/user/total_claimed_reward?userAddress=${e}`;
    return (await fetch(a).then((t) => t.json())).data;
  }
), De = _(
  async (e, a) => {
    const r = `https://open-api.naviprotocol.io/api/navi/user/rewards?userAddress=${e}&page=${(a == null ? void 0 : a.page) || 1}&pageSize=${(a == null ? void 0 : a.size) || 400}`, t = await fetch(r).then((c) => c.json());
    return B({
      data: t.data.rewards
    });
  }
);
async function Me(e, a, r) {
  var i;
  const t = await w({
    ...r,
    cacheTime: g
  }), c = await S({
    ...r,
    cacheTime: g
  }), n = /* @__PURE__ */ new Map();
  for (const u of a) {
    const { rewardCoinType: l, ruleIds: d } = u;
    for (const h of d) {
      n.has(l) || n.set(l, { assetIds: [], ruleIds: [], amount: 0 });
      const p = n.get(l);
      p.assetIds.push(u.assetCoinType.replace("0x", "")), p.ruleIds.push(h), p.amount += u.userClaimableReward;
    }
  }
  const o = [];
  for (const [u, { assetIds: l, ruleIds: d, amount: h }] of n) {
    const p = c.find(
      (v) => m(v.suiCoinType) === m(u)
    );
    if (!p || !p.contract.rewardFundId)
      throw new Error(`No matching rewardFund found for reward coin: ${u}`);
    const y = p.contract.rewardFundId;
    if (r != null && r.accountCap && !r.customCoinReceive)
      throw new Error("customCoinReceive is required when accountCap is provided");
    if (r != null && r.customCoinReceive) {
      let v;
      r.accountCap ? v = e.moveCall({
        target: `${t.package}::incentive_v3::claim_reward_with_account_cap`,
        arguments: [
          e.object("0x06"),
          // Clock object
          e.object(t.incentiveV3),
          // Incentive V3 contract
          e.object(t.storage),
          // Protocol storage
          e.object(y),
          // Reward fund
          e.pure.vector("string", l),
          // Asset IDs
          e.pure.vector("address", d),
          // Rule IDs
          f(r.accountCap, e.object)
          // Account capability
        ],
        typeArguments: [u]
      }) : v = e.moveCall({
        target: `${t.package}::incentive_v3::claim_reward`,
        arguments: [
          e.object("0x06"),
          // Clock object
          e.object(t.incentiveV3),
          // Incentive V3 contract
          e.object(t.storage),
          // Protocol storage
          e.object(y),
          // Reward fund
          e.pure.vector("string", l),
          // Asset IDs
          e.pure.vector("address", d)
          // Rule IDs
        ],
        typeArguments: [u]
      });
      const [j] = e.moveCall({
        target: "0x2::coin::from_balance",
        arguments: [v],
        typeArguments: [u]
      });
      if ((r == null ? void 0 : r.customCoinReceive.type) === "transfer") {
        if (!r.customCoinReceive.transfer)
          throw new Error("customCoinReceive.transfer is required");
        e.transferObjects(
          [j],
          f(r.customCoinReceive.transfer, e.pure.address)
        );
      }
      if ((r == null ? void 0 : r.customCoinReceive.type) === "depositNAVI") {
        const U = T(p.totalSupplyAmount).shiftedBy(-9), G = T(p.supplyCapCeiling).shiftedBy(-27);
        U.plus(h).isGreaterThan(G) && ((i = r == null ? void 0 : r.customCoinReceive.depositNAVI) != null && i.fallbackReceiveAddress) ? e.transferObjects(
          [j],
          e.pure.address(r.customCoinReceive.depositNAVI.fallbackReceiveAddress)
        ) : await x(e, p, j, r);
      } else
        o.push({
          coin: j,
          identifier: p
        });
    } else
      e.moveCall({
        target: `${t.package}::incentive_v3::claim_reward_entry`,
        arguments: [
          e.object("0x06"),
          // Clock object
          e.object(t.incentiveV3),
          // Incentive V3 contract
          e.object(t.storage),
          // Protocol storage
          e.object(y),
          // Reward fund
          e.pure.vector("string", l),
          // Asset IDs
          e.pure.vector("address", d)
          // Rule IDs
        ],
        typeArguments: [u]
      });
  }
  return o;
}
async function Ue(e, a) {
  const r = await w({
    ...a
  });
  return e.moveCall({
    target: `${r.package}::lending::create_account`,
    arguments: []
  });
}
export {
  R as Address,
  g as DEFAULT_CACHE_TIME,
  ge as FlashLoanAssetConfig,
  pe as IncentiveAPYInfo,
  J as IncentivePoolInfo,
  fe as IncentivePoolInfoByPhase,
  me as OracleInfo,
  P as PoolOperator,
  ye as ReserveDataInfo,
  Q as UserStateInfo,
  ve as borrowCoinPTB,
  Me as claimLendingRewardsPTB,
  Ue as createAccountCapPTB,
  x as depositCoinPTB,
  ke as filterPriceFeeds,
  Fe as flashloanPTB,
  F as getAllFlashLoanAssets,
  _e as getBorrowFee,
  Be as getCoins,
  w as getConfig,
  he as getFees,
  Se as getFlashLoanAsset,
  Ie as getHealthFactor,
  ee as getHealthFactorPTB,
  Te as getLendingState,
  C as getPool,
  S as getPools,
  re as getPriceFeeds,
  te as getPythStalePriceFeedId,
  Ae as getSimulatedHealthFactor,
  D as getSimulatedHealthFactorPTB,
  we as getStats,
  Pe as getTransactions,
  Ee as getUserAvailableLendingRewards,
  De as getUserClaimedRewardHistory,
  Le as getUserTotalClaimedReward,
  Ve as liquidatePTB,
  je as mergeCoinsPTB,
  m as normalizeCoinType,
  f as parseTxValue,
  Ce as repayCoinPTB,
  Re as repayFlashLoanPTB,
  Ne as summaryLendingRewards,
  $e as updateOraclePricesPTB,
  ae as updatePythPriceFeeds,
  A as withCache,
  _ as withSingleton,
  be as withdrawCoinPTB
};
//# sourceMappingURL=index.esm.js.map
