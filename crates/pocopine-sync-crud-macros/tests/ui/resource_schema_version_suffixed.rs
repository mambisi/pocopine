struct Customers;

#[pocopine_sync_crud::resource(name = "customers", schema_version = 3u64)]
impl Customers {}

fn main() {}
