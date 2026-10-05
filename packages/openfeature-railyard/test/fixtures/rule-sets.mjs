// QY1.3: twenty rule sets and a deterministic population, shared by the
// agreement tests (the SDK providers' local evaluation against the console's
// read endpoint and product analytics /flags, 10 000 ids each) and by the
// comparison with the reference SDK's own local evaluation.

export const COHORT_A = "0b8f6a3e-1c2d-4e5f-8a9b-0c1d2e3f4a5b";
export const COHORT_B = "7e6d5c4b-3a29-4817-a6b5-c4d3e2f1a0b9";

export const SEGMENTS = {
  paying: { key: "paying", name: "Paying", conditions: [{ type: "property", key: "plan", operator: "exact", value: ["pro", "team", "enterprise"] }] },
  eu_adults: {
    key: "eu_adults",
    name: "EU adults",
    conditions: [
      { type: "property", key: "country", operator: "exact", value: ["DE", "FR", "NL"] },
      { type: "property", key: "age", operator: "gte", value: 18 },
    ],
  },
};

const p = (key, operator, value) => (value === undefined ? { type: "property", key, operator } : { type: "property", key, operator, value });

/** Flag key -> {enabled, value, targeting}. `cohort` marks sets the reference SDK cannot evaluate locally. */
export const RULE_SETS = {
  r01_half: { enabled: true, value: null, targeting: { rules: [{ conditions: [], rollout: 50 }] } },
  r02_ten: { enabled: true, value: null, targeting: { rules: [{ conditions: [], rollout: 10 }] } },
  r03_third: { enabled: true, value: "blue", targeting: { rules: [{ conditions: [], rollout: 33.3 }] } },
  r04_pro: { enabled: true, value: null, targeting: { rules: [{ conditions: [p("plan", "exact", "pro")], rollout: 100 }] } },
  r05_paid_list: { enabled: true, value: null, targeting: { rules: [{ conditions: [p("plan", "exact", ["pro", "team"])], rollout: 60 }] } },
  r06_not_free: { enabled: true, value: null, targeting: { rules: [{ conditions: [p("plan", "is_not", "free")], rollout: 100 }] } },
  r07_company_mail: { enabled: true, value: null, targeting: { rules: [{ conditions: [p("email", "icontains", "@ACME.example")], rollout: 100 }] } },
  r08_not_test_mail: { enabled: true, value: null, targeting: { rules: [{ conditions: [p("email", "not_icontains", "test")], rollout: 40 }] } },
  r09_regex: { enabled: true, value: null, targeting: { rules: [{ conditions: [p("email", "regex", "^user_[0-9]*7@")], rollout: 100 }] } },
  r10_not_regex: { enabled: true, value: null, targeting: { rules: [{ conditions: [p("country", "not_regex", "^(US|CA)$")], rollout: 75 }] } },
  r11_age_gt: { enabled: true, value: null, targeting: { rules: [{ conditions: [p("age", "gt", 30)], rollout: 100 }] } },
  r12_age_lte_text: { enabled: true, value: null, targeting: { rules: [{ conditions: [p("age", "lte", "25")], rollout: 100 }] } },
  r13_beta_truthy: { enabled: true, value: null, targeting: { rules: [{ conditions: [p("beta", "exact", "true")], rollout: 100 }] } },
  r14_set: { enabled: true, value: null, targeting: { rules: [{ conditions: [p("company", "is_set")], rollout: 80 }] } },
  r15_segment: { enabled: true, value: null, targeting: { rules: [{ conditions: [{ type: "segment", key: "paying" }], rollout: 50 }] } },
  r16_two_rules: {
    enabled: true,
    value: "default",
    targeting: {
      rules: [
        { conditions: [p("plan", "exact", "enterprise")], rollout: 100, value: "enterprise" },
        { conditions: [{ type: "segment", key: "eu_adults" }], rollout: 30, value: "eu" },
        { conditions: [], rollout: 5 },
      ],
    },
  },
  r17_and: { enabled: true, value: null, targeting: { rules: [{ conditions: [p("plan", "is_not", "free"), p("age", "gte", 21), p("country", "exact", "DE")], rollout: 90 }] } },
  r18_off: { enabled: false, value: null, targeting: { rules: [{ conditions: [], rollout: 100 }] } },
  r19_cohort: { enabled: true, value: null, cohort: true, targeting: { rules: [{ conditions: [{ type: "cohort", id: COHORT_A }], rollout: 100 }, { conditions: [{ type: "cohort", id: COHORT_B }], rollout: 20 }] } },
  r20_cohort_and_prop: { enabled: true, value: null, cohort: true, targeting: { rules: [{ conditions: [{ type: "cohort", id: COHORT_A }, p("plan", "is_not", "free")], rollout: 70 }] } },
};

const PLANS = ["free", "pro", "team", "enterprise", "free"];
const COUNTRIES = ["DE", "FR", "US", "CA", "NL", "TR", "BR"];
const DOMAINS = ["acme.example", "shop.example", "test.example", "mail.example"];

/** The id of person i (several id shapes, as apps send them). */
export const idOf = (i) => (i % 3 === 0 ? `user_${i}` : i % 3 === 1 ? `${i}@shop.example` : `anon-${(i * 2654435761) % 4294967296}`);

/**
 * Person i's properties. `complete` gives every property (the reference SDK
 * asks its server when one is missing); otherwise some are left out.
 */
export function propsOf(i, complete = false) {
  const props = {
    plan: PLANS[i % PLANS.length],
    email: `user_${i}@${DOMAINS[(i >> 2) % DOMAINS.length]}`,
    country: COUNTRIES[(i * 7) % COUNTRIES.length],
    age: i % 11 === 0 ? String(16 + (i % 50)) : 16 + ((i * 13) % 50),
    beta: i % 4 === 0 ? true : i % 4 === 1 ? "TRUE" : i % 4 === 2 ? false : "false",
  };
  if (complete || i % 6 !== 0) props.company = `co_${i % 17}`;
  if (!complete && i % 9 === 0) delete props.country;
  return props;
}

/** The cohorts person i is in. */
export const cohortsOf = (i) => [...(i % 5 === 1 ? [COHORT_A] : []), ...(i % 3 === 0 ? [COHORT_B] : [])];
