// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `goalstate.rs`. The fixture follows WireServer's goal
//! state shape with placeholder identifiers.

#[cfg(test)]
mod tests {
    use super::super::*;

    const GOAL_STATE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<GoalState xmlns:i="http://www.w3.org/2001/XMLSchema-instance" xsi:noNamespaceSchemaLocation="goalstate10.xsd">
  <Version>2012-11-30</Version>
  <Incarnation>1</Incarnation>
  <Machine>
    <ExpectedState>Started</ExpectedState>
    <StopRolesDeadlineHint>300000</StopRolesDeadlineHint>
    <LBProbePorts><Port>16001</Port></LBProbePorts>
    <ExpectHealthReport>FALSE</ExpectHealthReport>
  </Machine>
  <Container>
    <ContainerId>00000000-0000-4000-8000-000000000001</ContainerId>
    <RoleInstanceList>
      <RoleInstance>
        <InstanceId>00000000000000000000000000000002._az-01</InstanceId>
        <State>Started</State>
        <Configuration>
          <HostingEnvironmentConfig>http://192.0.2.1:80/machine/x?comp=config&amp;type=hostingEnvironmentConfig&amp;incarnation=1</HostingEnvironmentConfig>
        </Configuration>
      </RoleInstance>
    </RoleInstanceList>
  </Container>
</GoalState>
"#;

    #[test]
    fn parses_the_three_fields() {
        let gs = parse_goal_state(GOAL_STATE.as_bytes()).expect("parsed");
        assert_eq!(gs.incarnation, "1");
        assert_eq!(gs.container_id, "00000000-0000-4000-8000-000000000001");
        assert_eq!(gs.instance_id, "00000000000000000000000000000002._az-01");
    }

    #[test]
    fn health_document_echoes_the_fields_and_says_ready() {
        let gs = parse_goal_state(GOAL_STATE.as_bytes()).expect("parsed");
        let doc = health_ready_document(&gs);
        assert!(doc.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?><Health "));
        assert!(doc.contains("<GoalStateIncarnation>1</GoalStateIncarnation>"));
        assert!(doc.contains("<ContainerId>00000000-0000-4000-8000-000000000001</ContainerId>"));
        assert!(doc.contains("<InstanceId>00000000000000000000000000000002._az-01</InstanceId>"));
        assert!(doc.contains("<Health><State>Ready</State></Health>"));
    }

    #[test]
    fn a_missing_field_is_refused() {
        let doc = GOAL_STATE.replace("<Incarnation>1</Incarnation>", "");
        assert_eq!(
            parse_goal_state(doc.as_bytes()),
            Err(GoalStateError::Missing("Incarnation"))
        );
        let doc = GOAL_STATE.replace(
            "<ContainerId>00000000-0000-4000-8000-000000000001</ContainerId>",
            "<ContainerId></ContainerId>",
        );
        assert_eq!(
            parse_goal_state(doc.as_bytes()),
            Err(GoalStateError::Missing("ContainerId"))
        );
    }

    #[test]
    fn a_value_that_could_inject_xml_is_refused() {
        let doc = GOAL_STATE.replace(
            "00000000000000000000000000000002._az-01",
            "x&lt;/InstanceId&gt;",
        );
        assert_eq!(
            parse_goal_state(doc.as_bytes()),
            Err(GoalStateError::Invalid("InstanceId"))
        );
        let doc = GOAL_STATE.replace("<Incarnation>1<", "<Incarnation>1 OR 2<");
        assert_eq!(
            parse_goal_state(doc.as_bytes()),
            Err(GoalStateError::Invalid("Incarnation"))
        );
    }

    #[test]
    fn an_overlong_value_is_refused() {
        let doc = GOAL_STATE.replace("00000000000000000000000000000002._az-01", &"a".repeat(200));
        assert_eq!(
            parse_goal_state(doc.as_bytes()),
            Err(GoalStateError::Invalid("InstanceId"))
        );
    }

    #[test]
    fn a_document_that_is_not_utf8_is_refused() {
        assert_eq!(parse_goal_state(b"\xff"), Err(GoalStateError::NotUtf8));
    }
}
