"""Reference vectors for A3's experiment statistics.

Runs the UNMODIFIED upstream Python (PostHog products/experiments/stats, MIT,
commit 645e1a78140757ea1bb9ddeb0ff9d3915c60b6f6) on the inputs of its own
test files and of GrowthBook's packages/stats tests (MIT, commit
44e8a69cf9cf213039801330dc420848df3ac831), plus scipy's chi-square test for
the sample-ratio check, and prints full-precision answers as JSON.

How it was run (2026-09-30), from a scratch directory outside the repo:
  gh api the files above into ./products/experiments/stats/** (with empty __init__.py files)
  uv venv /tmp/phvenv && uv pip install --python /tmp/phvenv/bin/python numpy scipy   (scipy 1.18.1)
  /tmp/phvenv/bin/python generate_vectors.py > vectors.json
"""

import json
import math

from scipy import stats as sps

from products.experiments.stats.bayesian.method import BayesianConfig, BayesianMethod
from products.experiments.stats.frequentist.method import FrequentistConfig, FrequentistMethod
from products.experiments.stats.frequentist.tests import SequentialTwoSidedTTest
from products.experiments.stats.shared.enums import DifferenceType
from products.experiments.stats.shared.statistics import ProportionStatistic, RatioStatistic, SampleMeanStatistic


def mean(s, ss, n):
    return {"kind": "mean", "sum": s, "sum_squares": ss, "n": n}


def prop(s, n):
    return {"kind": "proportion", "sum": s, "n": n}


def ratio(n, m, d, prod):
    return {"kind": "ratio", "n": n, "m": m, "d": d, "m_d_sum_of_products": prod}


def build(x):
    if x["kind"] == "mean":
        return SampleMeanStatistic(sum=x["sum"], sum_squares=x["sum_squares"], n=x["n"])
    if x["kind"] == "proportion":
        return ProportionStatistic(sum=x["sum"], n=x["n"])
    return RatioStatistic(n=x["n"], m_statistic=build(x["m"]), d_statistic=build(x["d"]), m_d_sum_of_products=x["m_d_sum_of_products"])


RATIO_T = ratio(2034, mean(99673.9364269569, 11298745.182728939, 2034), mean(947, 947, 2034), 99673.9364269569)
RATIO_C = ratio(1966, mean(94605.79858780127, 10463129.505392816, 1966), mean(936, 936, 1966), 94605.79858780127)

FREQUENTIST = [
    ("posthog sample mean", mean(1922.7, 94698.29, 2461), mean(1196.87, 37377.9767, 2507), "relative", None),
    ("posthog proportion", prop(62, 1471), prop(87, 1529), "relative", None),
    ("posthog ratio", RATIO_T, RATIO_C, "relative", None),
    ("posthog sequential sample mean, tuning 1000", mean(2500.0, 135000.0, 3500), mean(1400.0, 52000.0, 3000), "relative", 1000),
    ("posthog sequential proportion, tuning 5000", prop(2600, 3500), prop(1450, 3050), "relative", 5000),
    ("posthog sequential zero effect", prop(100, 2000), prop(100, 2000), "absolute", 5000),
    ("growthbook two-sided relative", mean(2422.7, 134698.29, 3461), mean(1396.87, 52377.9767, 3407), "relative", None),
    ("growthbook two-sided absolute", mean(2422.7, 134698.29, 3461), mean(1396.87, 52377.9767, 3407), "absolute", None),
    ("growthbook binomial", prop(16, 30), prop(14, 28), "relative", None),
]

BAYESIAN = [
    ("posthog basic mean absolute", mean(550, 3050, 100), mean(500, 2550, 100), "absolute", None),
    ("posthog proportion relative", prop(110, 1000), prop(100, 1000), "relative", None),
    ("posthog informative prior", mean(550, 3050, 100), mean(500, 2550, 100), "relative", (0.05, 0.01)),
    ("posthog realistic", mean(1922.7, 94698.29, 2461), mean(1196.87, 37377.9767, 2507), "relative", None),
    ("posthog ratio", RATIO_T, RATIO_C, "relative", None),
    ("posthog large numbers", mean(10500000, 110250001, 1000000), mean(10000000, 100000001, 1000000), "relative", None),
    ("growthbook binomial", prop(51, 100), prop(49, 100), "relative", None),
    ("growthbook gaussian", mean(105, 1111.5, 10), mean(100, 1002.25, 10), "relative", None),
    ("growthbook gaussian informative", mean(105, 1111.5, 10), mean(100, 1002.25, 10), "relative", (0.1, 0.1)),
]

SRM = [
    ("60/40 on 50/50", [600, 400], [0.5, 0.5]),
    ("503/497 on 50/50", [503, 497], [0.5, 0.5]),
    ("three arms", [3400, 3300, 3300], [1 / 3, 1 / 3, 1 / 3]),
    ("90/10 split kept", [9020, 980], [0.9, 0.1]),
]


def fnum(x):
    return None if x is None or (isinstance(x, float) and math.isnan(x)) else float(x)


out = {"frequentist": [], "bayesian": [], "srm": []}
for name, t, c, diff, tuning in FREQUENTIST:
    cfg = FrequentistConfig(alpha=0.05, difference_type=DifferenceType(diff), sequential_testing_enabled=tuning is not None, sequential_tuning_parameter=tuning or 5000.0)
    r = FrequentistMethod(cfg).run_test(build(t), build(c))
    out["frequentist"].append({
        "name": name, "treatment": t, "control": c, "difference": diff, "sequential_tuning": tuning,
        "point_estimate": fnum(r.point_estimate), "ci": [fnum(r.confidence_interval[0]), fnum(r.confidence_interval[1])],
        "p_value": fnum(r.p_value), "t": fnum(r.test_statistic), "df": fnum(r.degrees_of_freedom),
    })
for name, t, c, diff, prior in BAYESIAN:
    cfg = BayesianConfig(ci_level=0.95, difference_type=DifferenceType(diff), prior_mean=prior[0] if prior else 0.0, prior_variance=prior[1] if prior else 1.0, proper_prior=prior is not None)
    r = BayesianMethod(cfg).run_test(build(t), build(c))
    out["bayesian"].append({
        "name": name, "treatment": t, "control": c, "difference": diff, "prior": list(prior) if prior else None,
        "effect_size": fnum(r.effect_size), "ci": [fnum(r.credible_interval[0]), fnum(r.credible_interval[1])],
        "chance_to_win": fnum(r.chance_to_win), "posterior_variance": fnum(r.posterior_variance),
    })
for name, counts, weights in SRM:
    total = sum(counts)
    chi2, p = sps.chisquare(counts, [w * total for w in weights])
    out["srm"].append({"name": name, "counts": counts, "weights": weights, "chi2": float(chi2), "p_value": float(p)})

print(json.dumps(out, indent=1))
