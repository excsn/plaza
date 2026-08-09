use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;

/// Payload for an Op to create a new object with initial properties.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    ObjectId: Serialize + for<'de2> Deserialize<'de2>,
    PropertyKey: Serialize + for<'de2> Deserialize<'de2> + Eq + Hash,
    PropertyValue: Serialize + for<'de2> Deserialize<'de2>
")]
pub struct CreateObjectPayload<ObjectId, PropertyKey, PropertyValue>
where
    ObjectId: Clone + Debug + Eq + Hash,
    PropertyKey: Clone + Debug + Eq + Hash,
    PropertyValue: Clone + Debug,
{
    pub object_id: ObjectId,
    pub initial_properties: HashMap<PropertyKey, PropertyValue>,
}

/// Payload for an Op to delete an existing object.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "ObjectId: Serialize + for<'de2> Deserialize<'de2>")]
pub struct DeleteObjectPayload<ObjectId: Clone + Debug + Eq + Hash> {
    pub object_id: ObjectId,
}

/// Payload for an Op to set or update a specific property of an object.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    ObjectId: Serialize + for<'de2> Deserialize<'de2>,
    PropertyKey: Serialize + for<'de2> Deserialize<'de2> + Eq + Hash,
    PropertyValue: Serialize + for<'de2> Deserialize<'de2>
")]
pub struct SetObjectPropertyPayload<ObjectId, PropertyKey, PropertyValue>
where
    ObjectId: Clone + Debug + Eq + Hash,
    PropertyKey: Clone + Debug + Eq + Hash,
    PropertyValue: Clone + Debug,
{
    pub object_id: ObjectId,
    pub property_key: PropertyKey,
    pub value: PropertyValue,
}

/// Payload for an Op to delete a specific property from an object.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(bound = "
    ObjectId: Serialize + for<'de2> Deserialize<'de2>,
    PropertyKey: Serialize + for<'de2> Deserialize<'de2> + Eq + Hash
")]
pub struct DeleteObjectPropertyPayload<ObjectId, PropertyKey>
where
    ObjectId: Clone + Debug + Eq + Hash,
    PropertyKey: Clone + Debug + Eq + Hash,
{
    pub object_id: ObjectId,
    pub property_key: PropertyKey,
}
