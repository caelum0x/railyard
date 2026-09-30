// Cohort targeting for surveys. The SDKs show a survey only when its
// targeting flag is on for the person; for a cohort-targeted survey that flag
// is `survey-targeting-<id>` and is on exactly for the cohort's members
// (through merges). The /flags answer includes these flags once the flags
// route merges surveyTargetingAnswers (hand-off in docs/analytics/oss/A3.md);
// URL and slot-flag targeting need nothing beyond the survey definition.

import type { Db } from "../db.ts";
import { targetingFlagKey } from "./model.ts";

export interface TargetingAnswer {
  key: string;
  enabled: boolean;
  variant: null;
  payload: null;
}

export async function surveyTargetingAnswers(db: Db, projectId: string, distinctId: string): Promise<TargetingAnswer[]> {
  return db.asProject(projectId, async (c) => {
    const r = await c.query<{ id: string; member: boolean }>(
      `select s.id, exists (
         select 1 from pa.cohort_people cp
         left join pa.person_overrides o on o.project_id = cp.project_id and o.old_person_id = cp.person_id
         join pa.person_distinct_ids d on d.project_id = cp.project_id and d.distinct_id = $2
         where cp.project_id = s.project_id and cp.cohort_id = s.cohort_id and coalesce(o.new_person_id, cp.person_id) = d.person_id
       ) as member
       from pa.surveys s where s.project_id = $1 and s.status = 'running' and s.cohort_id is not null`,
      [projectId, distinctId],
    );
    return r.rows.map((x) => ({ key: targetingFlagKey(x.id), enabled: x.member, variant: null, payload: null }));
  });
}
