//! Разбор состава объекта из XML выгрузки.
//!
//! Фрагменты взяты из настоящей выгрузки УТ, а не придуманы: первая редакция
//! разбора искала `<Name>` прямым потомком `<Attribute>` — в реальном файле он
//! лежит внутри вложенного `<Properties>`, и разбор не находил ничего.

use lite_index::parse_object_xml_for_tests as parse;

/// Реквизит документа: раскладка как в `base/Documents/ЗаказКлиента.xml`.
const DOCUMENT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<MetaDataObject xmlns="http://v8.1c.ru/8.3/MDClasses">
  <Document uuid="1">
    <Properties>
      <Name>ЗаказКлиента</Name>
    </Properties>
    <ChildObjects>
      <Attribute uuid="2">
        <Properties>
          <Name>Контрагент</Name>
          <Synonym>
            <v8:item xmlns:v8="http://v8.1c.ru/8.1/data/core">
              <v8:lang>ru</v8:lang>
              <v8:content>Контрагент</v8:content>
            </v8:item>
          </Synonym>
          <ChoiceParameters>
            <app:item name="Отбор.Клиент" xmlns:app="http://v8.1c.ru/8.2/managed-application/core">
              <app:value>true</app:value>
            </app:item>
          </ChoiceParameters>
          <Indexing>DontIndex</Indexing>
        </Properties>
      </Attribute>
      <Attribute uuid="3">
        <Properties>
          <Name>Сделка</Name>
          <Indexing>Index</Indexing>
        </Properties>
      </Attribute>
      <TabularSection uuid="4">
        <Properties>
          <Name>Товары</Name>
        </Properties>
        <ChildObjects>
          <Attribute uuid="5">
            <Properties>
              <Name>Номенклатура</Name>
              <Indexing>Index</Indexing>
            </Properties>
          </Attribute>
        </ChildObjects>
      </TabularSection>
    </ChildObjects>
  </Document>
</MetaDataObject>"#;

/// Регистр накопления: `RegisterType` в свойствах объекта, измерения и ресурсы.
const REGISTER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<MetaDataObject xmlns="http://v8.1c.ru/8.3/MDClasses">
  <AccumulationRegister uuid="1">
    <Properties>
      <Name>ТоварыНаСкладах</Name>
      <RegisterType>Balance</RegisterType>
      <StandardAttributes>
        <xr:StandardAttribute name="LineNumber" xmlns:xr="http://v8.1c.ru/8.3/xcf/readable">
          <xr:FillChecking>DontCheck</xr:FillChecking>
        </xr:StandardAttribute>
      </StandardAttributes>
    </Properties>
    <ChildObjects>
      <Resource uuid="2">
        <Properties>
          <Name>ВНаличии</Name>
        </Properties>
      </Resource>
      <Dimension uuid="3">
        <Properties>
          <Name>Номенклатура</Name>
          <UseInTotals>true</UseInTotals>
        </Properties>
      </Dimension>
      <Dimension uuid="4">
        <Properties>
          <Name>Склад</Name>
        </Properties>
      </Dimension>
    </ChildObjects>
  </AccumulationRegister>
</MetaDataObject>"#;

#[test]
fn document_attributes_and_indexing() {
    let (register_type, fields) = parse(DOCUMENT_XML);
    assert_eq!(register_type, None, "у документа вида регистра быть не должно");

    let names: Vec<&str> = fields.iter().map(|(n, ..)| n.as_str()).collect();
    assert!(names.contains(&"Контрагент"), "{names:?}");
    assert!(names.contains(&"Сделка"), "{names:?}");

    let indexing = |name: &str| {
        fields
            .iter()
            .find(|(n, ..)| n == name)
            .and_then(|(_, _, ix)| ix.clone())
    };
    assert_eq!(indexing("Сделка").as_deref(), Some("Index"));
    // `DontIndex` не сохраняется: отсутствие значения читается однозначно.
    assert_eq!(indexing("Контрагент"), None);
}

#[test]
fn tabular_section_attributes_are_not_object_fields() {
    let (_, fields) = parse(DOCUMENT_XML);
    assert!(
        !fields.iter().any(|(n, ..)| n == "Номенклатура"),
        "реквизит табличной части попал в состав объекта: {fields:?}"
    );
}

#[test]
fn register_type_dimensions_and_resources() {
    let (register_type, fields) = parse(REGISTER_XML);
    assert_eq!(register_type.as_deref(), Some("Balance"));

    let of_kind = |kind: &str| -> Vec<&str> {
        fields
            .iter()
            .filter(|(_, k, _)| k == kind)
            .map(|(n, ..)| n.as_str())
            .collect()
    };
    assert_eq!(of_kind("resource"), vec!["ВНаличии"]);
    let dimensions = of_kind("dimension");
    assert!(dimensions.contains(&"Номенклатура"), "{dimensions:?}");
    assert!(dimensions.contains(&"Склад"), "{dimensions:?}");
}

#[test]
fn standard_attributes_are_not_fields() {
    // `<xr:StandardAttribute name="LineNumber">` — не реквизит объекта.
    let (_, fields) = parse(REGISTER_XML);
    assert!(
        !fields.iter().any(|(n, ..)| n == "LineNumber"),
        "стандартный реквизит попал в состав: {fields:?}"
    );
}

#[test]
fn broken_xml_does_not_panic() {
    let (_, fields) = parse("<MetaDataObject><Document><ChildObjects><Attribute");
    assert!(fields.is_empty());
}
