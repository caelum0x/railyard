// How queries see merged people: an event's person is its override target
// when one exists. A2's builders use these fragments; `e` is pa.events.

export const RESOLVED_PERSON_JOIN =
  "left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id";

export const RESOLVED_PERSON_ID = "coalesce(o.new_person_id, e.person_id)";
